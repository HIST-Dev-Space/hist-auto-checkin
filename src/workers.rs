use crate::{
    AppState,
    api::ScheduleRow,
    schedule::next_run,
    school::{CookieInput, PunchState, PunchTask, SchoolClient, SchoolError},
    weixin::WeixinError,
};
use chrono::{Duration, Utc};
use serde_json::Value;
use sqlx::{FromRow, types::Json};
use std::{sync::Arc, time::Duration as StdDuration};
use tokio::{
    sync::Semaphore,
    task::{JoinHandle, JoinSet},
};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

const CHECKIN_RETRY_DELAY_SECONDS: i64 = 60;
const CHECKIN_ATTEMPT_TIMEOUT: StdDuration = StdDuration::from_secs(180);
// The initial execution plus at most two retries.
const MAX_CHECKIN_ATTEMPTS: i32 = 3;

#[derive(Clone, FromRow)]
pub struct Job {
    pub id: Uuid,
    pub schedule_id: Uuid,
    pub user_id: Uuid,
    pub school_account_id: Uuid,
    pub attempts: i32,
    pub lease_token: Option<Uuid>,
}

pub fn start(state: AppState, cancel: CancellationToken) -> Vec<JoinHandle<()>> {
    vec![
        tokio::spawn(scheduler_loop(state.clone(), cancel.clone())),
        tokio::spawn(job_loop(state.clone(), cancel.clone())),
        tokio::spawn(notification_loop(state.clone(), cancel.clone())),
        tokio::spawn(poll_loop(state, cancel)),
    ]
}

async fn scheduler_loop(state: AppState, cancel: CancellationToken) {
    let mut ticker = tokio::time::interval(StdDuration::from_secs(2));
    loop {
        tokio::select! { _=cancel.cancelled()=>break, _=ticker.tick()=>{} }
        if let Err(error) = schedule_tick(&state).await {
            tracing::error!(%error, "scheduler tick failed");
        }
    }
}

pub async fn schedule_tick(state: &AppState) -> anyhow::Result<()> {
    let mut tx = state.db.begin().await?;
    // Serialize edits and occurrence creation, and allow multiple scheduler instances.
    let schedules: Vec<ScheduleRow> = sqlx::query_as("SELECT s.* FROM checkin_schedules s JOIN school_accounts a ON a.id=s.school_account_id WHERE s.enabled AND a.credential_status='active' AND s.next_run_at<=now() ORDER BY s.next_run_at FOR UPDATE OF s SKIP LOCKED LIMIT 100").fetch_all(&mut *tx).await?;
    for schedule in schedules {
        let next = next_run(
            schedule.local_time,
            &schedule.timezone,
            &schedule.weekdays,
            Utc::now(),
        )?;
        sqlx::query("INSERT INTO checkin_jobs(id,schedule_id,user_id,school_account_id,scheduled_at,available_at) VALUES($1,$2,$3,$4,$5,now()) ON CONFLICT(schedule_id,scheduled_at) DO NOTHING")
            .bind(Uuid::new_v4()).bind(schedule.id).bind(schedule.user_id).bind(schedule.school_account_id).bind(schedule.next_run_at).execute(&mut *tx).await?;
        sqlx::query("UPDATE checkin_schedules SET next_run_at=$1 WHERE id=$2")
            .bind(next)
            .bind(schedule.id)
            .execute(&mut *tx)
            .await?;
    }
    sqlx::query("UPDATE checkin_jobs SET status='pending',lease_token=NULL,lease_until=NULL WHERE status='running' AND lease_until<=now()").execute(&mut *tx).await?;
    sqlx::query("UPDATE checkin_jobs j SET status='cancelled',finished_at=now(),result_message='自动签到已关闭' FROM checkin_schedules s WHERE j.schedule_id=s.id AND NOT s.enabled AND j.status='pending'").execute(&mut *tx).await?;
    sqlx::query("UPDATE school_accounts SET execution_token=NULL,execution_until=NULL WHERE execution_until<=now()").execute(&mut *tx).await?;
    let exhausted: Vec<(Uuid, Uuid, String)> = sqlx::query_as("UPDATE checkin_jobs SET status='failed',result_message=COALESCE(NULLIF(result_message,''),'未能完成签到'),finished_at=now() WHERE status='pending' AND attempts>=$1 RETURNING id,user_id,result_message")
        .bind(MAX_CHECKIN_ATTEMPTS).fetch_all(&mut *tx).await?;
    for (job, user, message) in exhausted {
        enqueue(&mut tx, user, job, &message).await?;
    }
    sqlx::query("UPDATE notification_outbox SET status='pending',lease_token=NULL,lease_until=NULL WHERE status='sending' AND lease_until<=now()").execute(&mut *tx).await?;
    sqlx::query("UPDATE notification_outbox SET status='failed',last_error=COALESCE(last_error,'通知已过期') WHERE status='pending' AND expires_at<=now()").execute(&mut *tx).await?;
    sqlx::query("DELETE FROM sessions WHERE expires_at<now()")
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM weixin_login_sessions WHERE expires_at<now()-interval '1 day'")
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM school_login_sessions WHERE expires_at<now()")
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM auth_buckets WHERE expires_at<now()-interval '1 hour'")
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

async fn enqueue(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    user: Uuid,
    job: Uuid,
    message: &str,
) -> anyhow::Result<()> {
    let title: String = sqlx::query_scalar("SELECT task_title FROM checkin_jobs WHERE id=$1")
        .bind(job)
        .fetch_one(&mut **tx)
        .await?;
    let title = title.trim();
    let body = if title.is_empty() {
        format!("Hist-Auto-Checkin\n{message}")
    } else {
        format!("Hist-Auto-Checkin\n{title}\n{message}")
    };
    sqlx::query("INSERT INTO notification_outbox(id,user_id,job_id,body) VALUES($1,$2,$3,$4) ON CONFLICT(job_id) DO NOTHING").bind(Uuid::new_v4()).bind(user).bind(job).bind(body).execute(&mut **tx).await?;
    Ok(())
}

pub async fn claim_job(state: &AppState) -> anyhow::Result<Option<(Job, ScheduleRow, String)>> {
    let mut tx = state.db.begin().await?;
    let job: Option<Job> = sqlx::query_as("SELECT j.* FROM checkin_jobs j JOIN school_accounts a ON a.id=j.school_account_id JOIN checkin_schedules s ON s.id=j.schedule_id WHERE j.status='pending' AND j.attempts<$1 AND j.available_at<=now() AND s.enabled AND a.credential_status='active' AND (a.execution_until IS NULL OR a.execution_until<=now()) ORDER BY j.available_at FOR UPDATE OF j,a,s SKIP LOCKED LIMIT 1").bind(MAX_CHECKIN_ATTEMPTS).fetch_optional(&mut *tx).await?;
    let Some(mut job) = job else {
        return Ok(None);
    };
    let token = Uuid::new_v4();
    job.attempts = sqlx::query_scalar("UPDATE checkin_jobs SET status='running',attempts=attempts+1,lease_token=$1,lease_until=now()+interval '120 seconds' WHERE id=$2 RETURNING attempts").bind(token).bind(job.id).fetch_one(&mut *tx).await?;
    let cookies: String = sqlx::query_scalar("UPDATE school_accounts SET execution_token=$1,execution_until=now()+interval '120 seconds' WHERE id=$2 RETURNING cookies_ciphertext").bind(token).bind(job.school_account_id).fetch_one(&mut *tx).await?;
    let schedule = sqlx::query_as("SELECT * FROM checkin_schedules WHERE id=$1")
        .bind(job.schedule_id)
        .fetch_one(&mut *tx)
        .await?;
    tx.commit().await?;
    job.lease_token = Some(token);
    Ok(Some((job, schedule, cookies)))
}

async fn job_loop(state: AppState, cancel: CancellationToken) {
    let mut tasks = JoinSet::new();
    let mut ticker = tokio::time::interval(StdDuration::from_secs(1));
    loop {
        tokio::select! {
            _=cancel.cancelled()=>break,
            Some(result)=tasks.join_next(), if !tasks.is_empty()=>{ if let Err(e)=result { tracing::error!(error=%e,"job task failed"); } },
            _=ticker.tick()=>{
                while tasks.len()<state.config.worker_concurrency {
                    match claim_job(&state).await {
                        Ok(Some(claim))=>{ tasks.spawn(run_job(state.clone(),claim)); },
                        Ok(None)=>break,
                        Err(e)=>{ tracing::error!(error=%e,"job claim failed"); break; }
                    }
                }
            }
        }
    }
    let drain = async { while tasks.join_next().await.is_some() {} };
    if tokio::time::timeout(StdDuration::from_secs(30), drain)
        .await
        .is_err()
    {
        tasks.shutdown().await;
    }
}

enum Outcome {
    Done(bool, String),
    Retry(String),
    Invalid,
}

async fn run_job(state: AppState, (job, schedule, ciphertext): (Job, ScheduleRow, String)) {
    let stop = CancellationToken::new();
    let _stop_guard = stop.clone().drop_guard();
    let lost = CancellationToken::new();
    let heartbeat = tokio::spawn(heartbeat(
        state.clone(),
        job.clone(),
        stop.clone(),
        lost.clone(),
    ));
    let outcome = tokio::select! {
        _=lost.cancelled()=>None,
        result=tokio::time::timeout(CHECKIN_ATTEMPT_TIMEOUT,process_job(&state,&job,&schedule,&ciphertext))=>Some(match result {
            Ok(Ok(outcome))=>outcome,
            Ok(Err(error))=>{ tracing::error!(job_id=%job.id,%error,"job processing failed"); Outcome::Retry("执行服务暂时不可用".into()) },
            Err(_)=>Outcome::Retry("本次签到执行超时".into()),
        })
    };
    stop.cancel();
    let _ = heartbeat.await;
    if let Some(outcome) = outcome
        && let Err(error) = finish_job(&state, &job, outcome).await
    {
        tracing::error!(job_id=%job.id,%error,"job completion failed");
    }
}

async fn heartbeat(state: AppState, job: Job, stop: CancellationToken, lost: CancellationToken) {
    let mut ticker = tokio::time::interval(StdDuration::from_secs(20));
    loop {
        tokio::select! { _=stop.cancelled()=>break, _=ticker.tick()=>{} }
        let result: anyhow::Result<bool> = async {
            let mut tx=state.db.begin().await?;
            let count=sqlx::query("UPDATE checkin_jobs SET lease_until=now()+interval '120 seconds' WHERE id=$1 AND lease_token=$2 AND status='running' AND lease_until>now()").bind(job.id).bind(job.lease_token).execute(&mut *tx).await?.rows_affected();
            let accounts=sqlx::query("UPDATE school_accounts SET execution_until=now()+interval '120 seconds' WHERE id=$1 AND execution_token=$2 AND execution_until>now()").bind(job.school_account_id).bind(job.lease_token).execute(&mut *tx).await?.rows_affected();
            if count!=1 || accounts!=1 { return Ok(false); }
            tx.commit().await?;
            Ok(true)
        }.await;
        if !matches!(result, Ok(true)) {
            lost.cancel();
            break;
        }
    }
}

async fn process_job(
    state: &AppState,
    job: &Job,
    schedule: &ScheduleRow,
    ciphertext: &str,
) -> anyhow::Result<Outcome> {
    let raw = state.secrets.decrypt(
        ciphertext,
        &format!("school:{}:{}", job.user_id, job.school_account_id),
    )?;
    let cookies: Vec<CookieInput> = serde_json::from_str(&raw)?;
    let client = SchoolClient::new_with_proxy(
        &state.config.school_base_url,
        &cookies,
        state.config.school_proxy_url.as_deref(),
    )?;
    let result = process_with_client(state, job, schedule, &client).await;
    let refreshed = state.secrets.encrypt(
        &serde_json::to_string(&client.refreshed_cookies())?,
        &format!("school:{}:{}", job.user_id, job.school_account_id),
    )?;
    sqlx::query("UPDATE school_accounts SET cookies_ciphertext=$1 WHERE id=$2 AND execution_token=$3 AND execution_until>now()").bind(refreshed).bind(job.school_account_id).bind(job.lease_token).execute(&state.db).await?;
    result
}

async fn process_with_client(
    state: &AppState,
    job: &Job,
    schedule: &ScheduleRow,
    client: &SchoolClient,
) -> anyhow::Result<Outcome> {
    let mut tasks = match client.tasks(&schedule.course_ids).await {
        Ok(tasks) => tasks,
        Err(SchoolError::CredentialsInvalid) => return Ok(Outcome::Invalid),
        Err(SchoolError::Rejected(message)) => return Ok(Outcome::Done(false, message)),
        Err(error) => return Ok(Outcome::Retry(error.to_string())),
    };
    // Recover intents even when the school no longer lists a task as active.
    let uncertain: Vec<(String,String,String,String)> = sqlx::query_as("SELECT course_id,punch_id,punch_url,task_title FROM checkin_records WHERE school_account_id=$1 AND ($3 OR course_id=ANY($2)) AND status IN ('submitting','uncertain')").bind(job.school_account_id).bind(&schedule.course_ids).bind(schedule.course_ids.is_empty()).fetch_all(&state.db).await?;
    for (course_id, punch_id, url, title) in uncertain {
        if !tasks
            .iter()
            .any(|t| t.course_id == course_id && t.punch_id == punch_id)
        {
            let url = reqwest::Url::parse(&url)?;
            anyhow::ensure!(
                url.origin() == reqwest::Url::parse(&state.config.school_base_url)?.origin(),
                "invalid saved school origin"
            );
            tasks.push(PunchTask {
                course_id,
                punch_id,
                title,
                url,
            });
        }
    }
    if tasks.is_empty() {
        return Ok(Outcome::Retry("当前尚未开放签到".into()));
    }
    let title = tasks
        .iter()
        .map(|task| task.title.trim())
        .filter(|title| !title.is_empty())
        .collect::<Vec<_>>()
        .join("；");
    sqlx::query("UPDATE checkin_jobs SET task_title=$1 WHERE id=$2 AND lease_token=$3 AND status='running' AND lease_until>now()")
        .bind(title).bind(job.id).bind(job.lease_token).execute(&state.db).await?;
    let mut messages = Vec::new();
    let mut failures = false;
    let mut pending = false;
    for task in tasks {
        let prior: Option<String>=sqlx::query_scalar("SELECT status FROM checkin_records WHERE school_account_id=$1 AND course_id=$2 AND punch_id=$3").bind(job.school_account_id).bind(&task.course_id).bind(&task.punch_id).fetch_optional(&state.db).await?;
        if prior.as_deref() == Some("succeeded") {
            messages.push("已签到".to_owned());
            continue;
        }
        let current = match client.state(&task).await {
            Ok(current) => current,
            Err(SchoolError::CredentialsInvalid) => return Ok(Outcome::Invalid),
            Err(error) => {
                pending = true;
                messages.push(error.to_string());
                continue;
            }
        };
        let (status, message) = match current {
            PunchState::AlreadyDone => ("succeeded", "已签到".to_owned()),
            PunchState::Closed => {
                failures = true;
                ("failed", "签到已结束，未确认成功".into())
            }
            PunchState::Ready if matches!(prior.as_deref(), Some("submitting" | "uncertain")) => {
                pending = true;
                messages.push("提交结果待确认".into());
                continue;
            }
            PunchState::Ready => {
                save_record(state, job, schedule, &task, "submitting", "正在提交签到").await?;
                ensure_owned(state, job).await?;
                match client
                    .submit(&task, schedule.latitude, schedule.longitude)
                    .await
                {
                    Ok(()) => ("succeeded", "签到成功".to_owned()),
                    Err(SchoolError::CredentialsInvalid) => return Ok(Outcome::Invalid),
                    Err(SchoolError::Rejected(message)) => {
                        failures = true;
                        ("failed", message)
                    }
                    Err(_) => {
                        pending = true;
                        ("uncertain", "提交结果待确认，正在查询学校状态".into())
                    }
                }
            }
        };
        save_record(state, job, schedule, &task, status, &message).await?;
        messages.push(message);
    }
    let message = messages.join("；");
    if pending {
        Ok(Outcome::Retry(message))
    } else {
        Ok(Outcome::Done(!failures, message))
    }
}

async fn ensure_owned(state: &AppState, job: &Job) -> anyhow::Result<()> {
    let owned: bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM checkin_jobs WHERE id=$1 AND lease_token=$2 AND status='running' AND lease_until>now())").bind(job.id).bind(job.lease_token).fetch_one(&state.db).await?;
    anyhow::ensure!(owned, "execution lease lost");
    Ok(())
}

async fn save_record(
    state: &AppState,
    job: &Job,
    schedule: &ScheduleRow,
    task: &PunchTask,
    status: &str,
    message: &str,
) -> anyhow::Result<()> {
    let mut tx = state.db.begin().await?;
    let owned: Option<Uuid>=sqlx::query_scalar("SELECT id FROM checkin_jobs WHERE id=$1 AND lease_token=$2 AND status='running' AND lease_until>now() FOR UPDATE").bind(job.id).bind(job.lease_token).fetch_optional(&mut *tx).await?;
    anyhow::ensure!(owned.is_some(), "execution lease lost");
    sqlx::query("INSERT INTO checkin_records(school_account_id,course_id,punch_id,punch_url,task_title,job_id,status,message,latitude,longitude) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10) ON CONFLICT(school_account_id,course_id,punch_id) DO UPDATE SET task_title=EXCLUDED.task_title,job_id=EXCLUDED.job_id,status=EXCLUDED.status,message=EXCLUDED.message,latitude=EXCLUDED.latitude,longitude=EXCLUDED.longitude,updated_at=now() WHERE checkin_records.status<>'succeeded'")
        .bind(job.school_account_id).bind(&task.course_id).bind(&task.punch_id).bind(task.url.as_str()).bind(&task.title).bind(job.id).bind(status).bind(message).bind(schedule.latitude).bind(schedule.longitude).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(())
}

async fn finish_job(state: &AppState, job: &Job, outcome: Outcome) -> anyhow::Result<()> {
    let mut tx = state.db.begin().await?;
    let owned: Option<Uuid>=sqlx::query_scalar("SELECT id FROM checkin_jobs WHERE id=$1 AND lease_token=$2 AND status='running' AND lease_until>now() FOR UPDATE").bind(job.id).bind(job.lease_token).fetch_optional(&mut *tx).await?;
    if owned.is_none() {
        return Ok(());
    }
    let retry_at = Utc::now() + Duration::seconds(CHECKIN_RETRY_DELAY_SECONDS);
    let (status, message, terminal) = match outcome {
        Outcome::Done(success, message) => {
            (if success { "succeeded" } else { "failed" }, message, true)
        }
        Outcome::Retry(message) if job.attempts >= MAX_CHECKIN_ATTEMPTS => {
            ("failed", message, true)
        }
        Outcome::Retry(message) => ("pending", message, false),
        Outcome::Invalid => {
            sqlx::query("UPDATE school_accounts SET credential_status='invalid' WHERE id=$1")
                .bind(job.school_account_id)
                .execute(&mut *tx)
                .await?;
            sqlx::query("UPDATE checkin_schedules SET enabled=false WHERE school_account_id=$1")
                .bind(job.school_account_id)
                .execute(&mut *tx)
                .await?;
            sqlx::query("UPDATE checkin_jobs SET status='cancelled',finished_at=now(),result_message='学校凭证失效，已暂停规则' WHERE school_account_id=$1 AND status='pending'").bind(job.school_account_id).execute(&mut *tx).await?;
            (
                "failed",
                "学校登录已失效，规则已暂停，请重新登录后启用任务".into(),
                true,
            )
        }
    };
    sqlx::query("UPDATE checkin_jobs SET status=$1,result_message=$2,available_at=$4,lease_token=NULL,lease_until=NULL,finished_at=CASE WHEN $3 THEN now() ELSE NULL END WHERE id=$5").bind(status).bind(&message).bind(terminal).bind(retry_at).bind(job.id).execute(&mut *tx).await?;
    sqlx::query("UPDATE school_accounts SET execution_token=NULL,execution_until=NULL WHERE id=$1 AND execution_token=$2").bind(job.school_account_id).bind(job.lease_token).execute(&mut *tx).await?;
    if terminal {
        enqueue(&mut tx, job.user_id, job.id, &message).await?;
    }
    tx.commit().await?;
    tracing::info!(job_id=%job.id,status,"checkin job updated");
    Ok(())
}

#[derive(Clone, FromRow)]
struct Binding {
    user_id: Uuid,
    recipient_id: String,
    token_ciphertext: String,
    context_ciphertext: Option<String>,
    cursor_ciphertext: Option<String>,
    base_url: String,
    poll_token: Option<Uuid>,
}

async fn poll_loop(state: AppState, cancel: CancellationToken) {
    let permits = Arc::new(Semaphore::new(state.config.poll_concurrency));
    let mut tasks = JoinSet::new();
    let mut ticker = tokio::time::interval(StdDuration::from_secs(1));
    loop {
        tokio::select! {
            _=cancel.cancelled()=>break,
            Some(_)=tasks.join_next(),if !tasks.is_empty()=>{},
            _=ticker.tick()=>{
                while let Ok(permit)=permits.clone().try_acquire_owned() {
                    match claim_binding(&state).await {
                        Ok(Some(binding))=>{
                            let state=state.clone();
                            let cancel=cancel.clone();
                            tasks.spawn(async move {
                                let _permit=permit;
                                tokio::select! {
                                    _=cancel.cancelled()=>{},
                                    result=poll_binding(&state,&binding)=>{ if let Err(error)=result { tracing::error!(user_id=%binding.user_id,%error,"weixin poll persistence failed"); } }
                                }
                            });
                        },
                        Ok(None)=>break,
                        Err(error)=>{ tracing::error!(%error,"weixin binding claim failed"); break; }
                    }
                }
            }
        }
    }
    while tasks.join_next().await.is_some() {}
}

async fn claim_binding(state: &AppState) -> anyhow::Result<Option<Binding>> {
    let token = Uuid::new_v4();
    Ok(sqlx::query_as("UPDATE weixin_bindings SET poll_token=$1,poll_until=now()+interval '60 seconds' WHERE user_id=(SELECT user_id FROM weixin_bindings WHERE status='active' AND poll_available_at<=now() AND (poll_until IS NULL OR poll_until<=now()) ORDER BY poll_available_at FOR UPDATE SKIP LOCKED LIMIT 1) RETURNING *").bind(token).fetch_optional(&state.db).await?)
}

async fn poll_binding(state: &AppState, binding: &Binding) -> anyhow::Result<()> {
    let token = state.secrets.decrypt(
        &binding.token_ciphertext,
        &format!("weixin-token:{}", binding.user_id),
    )?;
    let cursor = binding
        .cursor_ciphertext
        .as_ref()
        .map(|s| {
            state
                .secrets
                .decrypt(s, &format!("weixin-cursor:{}", binding.user_id))
        })
        .transpose()?
        .unwrap_or_default();
    match state
        .weixin
        .updates(&binding.base_url, &token, &cursor)
        .await
    {
        Ok(updates) => {
            let context = updates
                .msgs
                .into_iter()
                .filter(|m| {
                    m.from_user_id.as_deref() == Some(&binding.recipient_id)
                        && m.message_type == Some(1)
                })
                .filter_map(|m| m.context_token)
                .rfind(|s| !s.is_empty());
            let encrypted_context = context
                .map(|s| {
                    state
                        .secrets
                        .encrypt(&s, &format!("weixin-context:{}", binding.user_id))
                })
                .transpose()?;
            let encrypted_cursor = updates
                .get_updates_buf
                .filter(|s| !s.is_empty())
                .map(|s| {
                    state
                        .secrets
                        .encrypt(&s, &format!("weixin-cursor:{}", binding.user_id))
                })
                .transpose()?;
            let mut tx = state.db.begin().await?;
            let changed=sqlx::query("UPDATE weixin_bindings SET context_ciphertext=COALESCE($1,context_ciphertext),context_updated_at=CASE WHEN $1::text IS NOT NULL THEN now() ELSE context_updated_at END,cursor_ciphertext=COALESCE($2,cursor_ciphertext),poll_token=NULL,poll_until=NULL,poll_available_at=now()+interval '1 second',last_error=NULL WHERE user_id=$3 AND poll_token=$4")
                .bind(&encrypted_context).bind(encrypted_cursor).bind(binding.user_id).bind(binding.poll_token).execute(&mut *tx).await?.rows_affected();
            if changed == 1 && encrypted_context.is_some() {
                sqlx::query("UPDATE notification_outbox SET available_at=now() WHERE user_id=$1 AND status='pending' AND expires_at>now() AND last_error='等待微信绑定或用户发送消息以建立会话'").bind(binding.user_id).execute(&mut *tx).await?;
            }
            tx.commit().await?;
        }
        Err(error) => {
            let expired = matches!(error, WeixinError::Api(-14));
            sqlx::query("UPDATE weixin_bindings SET status=CASE WHEN $1 THEN 'expired' ELSE status END,poll_token=NULL,poll_until=NULL,poll_available_at=now()+interval '30 seconds',last_error=$2 WHERE user_id=$3 AND poll_token=$4").bind(expired).bind(error.to_string()).bind(binding.user_id).bind(binding.poll_token).execute(&state.db).await?;
        }
    }
    Ok(())
}

#[derive(FromRow)]
struct Notification {
    id: Uuid,
    user_id: Uuid,
    body: String,
    attempts: i32,
    lease_token: Option<Uuid>,
}

async fn notification_loop(state: AppState, cancel: CancellationToken) {
    let mut ticker = tokio::time::interval(StdDuration::from_secs(1));
    let mut tasks = JoinSet::new();
    loop {
        tokio::select! {
            _=cancel.cancelled()=>break,
            Some(_)=tasks.join_next(),if !tasks.is_empty()=>{},
            _=ticker.tick()=>{
                while tasks.len()<state.config.worker_concurrency {
                    let token=Uuid::new_v4();
                    let claim=sqlx::query_as::<_,Notification>("UPDATE notification_outbox SET status='sending',lease_token=$1,lease_until=now()+interval '60 seconds' WHERE id=(SELECT id FROM notification_outbox WHERE status='pending' AND available_at<=now() AND expires_at>now() ORDER BY available_at FOR UPDATE SKIP LOCKED LIMIT 1) RETURNING *").bind(token).fetch_optional(&state.db).await;
                    match claim {
                        Ok(Some(notification))=>{
                            let state=state.clone();
                            tasks.spawn(async move { if let Err(error)=send_notification(&state,&notification).await { tracing::error!(%error,notification_id=%notification.id,"notification persistence failed"); } });
                        },
                        Ok(None)=>break,
                        Err(error)=>{ tracing::error!(%error,"notification claim failed"); break; }
                    }
                }
            }
        }
    }
    while tasks.join_next().await.is_some() {}
}

async fn send_notification(state: &AppState, notification: &Notification) -> anyhow::Result<()> {
    let binding: Option<Binding> =
        sqlx::query_as("SELECT * FROM weixin_bindings WHERE user_id=$1 AND status='active'")
            .bind(notification.user_id)
            .fetch_optional(&state.db)
            .await?;
    let Some(binding) = binding.filter(|b| b.context_ciphertext.is_some()) else {
        sqlx::query("UPDATE notification_outbox SET status='pending',available_at=now()+interval '60 seconds',last_error='等待微信绑定或用户发送消息以建立会话',lease_token=NULL,lease_until=NULL WHERE id=$1 AND lease_token=$2").bind(notification.id).bind(notification.lease_token).execute(&state.db).await?;
        return Ok(());
    };
    let token = state.secrets.decrypt(
        &binding.token_ciphertext,
        &format!("weixin-token:{}", binding.user_id),
    )?;
    let context = state.secrets.decrypt(
        binding.context_ciphertext.as_deref().unwrap(),
        &format!("weixin-context:{}", binding.user_id),
    )?;
    let result = state
        .weixin
        .send_text(
            &binding.base_url,
            &token,
            &binding.recipient_id,
            &context,
            &format!("hist-{}", notification.id),
            &notification.body,
        )
        .await;
    match result {
        Ok(()) => {
            sqlx::query("UPDATE notification_outbox SET status='sent',sent_at=now(),attempts=attempts+1,lease_token=NULL,lease_until=NULL,last_error=NULL WHERE id=$1 AND lease_token=$2").bind(notification.id).bind(notification.lease_token).execute(&state.db).await?;
        }
        Err(error) => {
            let expired = matches!(error, WeixinError::Api(-14));
            let attempts = notification.attempts + 1;
            let delay = (30_i64 * 2_i64.pow(attempts.min(7) as u32)).min(3600);
            let mut tx = state.db.begin().await?;
            sqlx::query("UPDATE notification_outbox SET status=CASE WHEN $1>=8 THEN 'failed' ELSE 'pending' END,attempts=$1,available_at=$2,last_error=$3,lease_token=NULL,lease_until=NULL WHERE id=$4 AND lease_token=$5").bind(attempts).bind(Utc::now()+Duration::seconds(delay)).bind(error.to_string()).bind(notification.id).bind(notification.lease_token).execute(&mut *tx).await?;
            if expired {
                sqlx::query("UPDATE weixin_bindings SET status='expired',last_error=$1 WHERE user_id=$2 AND token_ciphertext=$3").bind(error.to_string()).bind(binding.user_id).bind(binding.token_ciphertext).execute(&mut *tx).await?;
            }
            tx.commit().await?;
        }
    }
    Ok(())
}

pub async fn queue_snapshot(state: &AppState) -> anyhow::Result<Value> {
    Ok(sqlx::query_scalar::<_,Json<Value>>("SELECT jsonb_build_object('pending',(SELECT count(*) FROM checkin_jobs WHERE status='pending'),'running',(SELECT count(*) FROM checkin_jobs WHERE status='running'))").fetch_one(&state.db).await?.0)
}
