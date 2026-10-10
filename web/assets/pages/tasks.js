import { api } from '../common/api.js';
import { $, busy, showError } from '../common/dom.js';
import { enhanceSelect } from '../components/select.js';

export async function init() {
  const controller = new AbortController();
  const requestOptions = { signal: controller.signal };
  window.addEventListener('pagehide', () => controller.abort(), { once: true });
  const params = new URLSearchParams(location.search);
  let setup = params.get('setup') === '1';
  let account;
  let places = [];
  let editing;
  let saving = false;
  const saveButton = $('#save-schedule');
  const timeInput = $('#local-time');
  const locationSelect = $('#checkin-location');
  saveButton.textContent = setup ? '下一步' : '确认';

  const chosenPlace = () => places.find((place) => place.id === locationSelect.value);
  function updateForm() {
    saveButton.disabled =
      saving || account?.credential_status !== 'active' || !timeInput.value || !chosenPlace();
  }
  timeInput.addEventListener('input', updateForm);
  locationSelect.addEventListener('change', updateForm);
  $('#schedule-form').addEventListener('submit', async (event) => {
    event.preventDefault();
    if (saving) return;
    showError('#page-error', '');
    try {
      const place = chosenPlace();
      if (account?.credential_status !== 'active' || !place) {
        throw new Error('请确认学校登录有效，并选择签到地点。');
      }
      const payload = {
        school_account_id: account.id,
        local_time: timeInput.value,
        latitude: place.latitude,
        longitude: place.longitude,
        location_name: place.name,
      };
      if (editing) payload.enabled = editing.enabled;
      saving = true;
      await busy(saveButton, '确认中', async () => {
        await api(editing ? `/api/schedules/${editing.id}` : '/api/schedules', {
          method: editing ? 'PUT' : 'POST',
          body: payload,
        });
        location.assign(setup ? '/notifications?setup=1' : '/account?saved=schedule');
      });
    } catch (error) {
      showError('#page-error', error.message);
    } finally {
      saving = false;
      updateForm();
    }
  });

  await api('/api/me', requestOptions);
  const [accounts, locations, rules] = await Promise.all([
    api('/api/school-accounts', requestOptions),
    api('/api/locations', requestOptions),
    api('/api/schedules', requestOptions),
  ]);
  places = locations.filter((place) => place.kind === 'dorm');
  if (!accounts.length) {
    location.replace('/login?reconnect=1');
    return;
  }
  editing = params.has('edit')
    ? rules.find((rule) => rule.id === params.get('edit'))
    : rules.at(-1);
  if (params.has('edit') && !editing) throw new Error('签到设置不存在或已删除，请重新选择。');
  setup ||= !editing;
  saveButton.textContent = setup ? '下一步' : '确认';
  account =
    (editing && accounts.find((value) => value.id === editing.school_account_id)) ||
    accounts.find((value) => value.credential_status === 'active') ||
    accounts[0];

  let selected;
  if (editing) {
    selected = places.find(
      (place) =>
        place.name === editing.location_name &&
        Math.abs(place.latitude - editing.latitude) < 0.000001 &&
        Math.abs(place.longitude - editing.longitude) < 0.000001,
    );
    timeInput.value = editing.local_time.slice(0, 5);
  } else {
    timeInput.value = '21:10';
  }
  locationSelect.replaceChildren(
    new Option('请选择宿舍楼', ''),
    ...places.map((place) => new Option(place.name, place.id)),
  );
  if (selected) locationSelect.value = selected.id;
  timeInput.disabled = false;
  locationSelect.disabled = false;
  enhanceSelect(locationSelect);
  if (account.credential_status !== 'active')
    showError('#page-error', '学校登录已失效，请重新登录。');
  updateForm();
}
