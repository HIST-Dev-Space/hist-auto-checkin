import { $ } from '../common/dom.js';

export function celebrateSetup() {
  const dialog = $('#setup-success'),
    canvas = $('#celebration-canvas');
  dialog.showModal();
  $('#enter-account').focus();
  let frame, navigation;
  const enter = () => {
    clearTimeout(navigation);
    cancelAnimationFrame(frame);
    location.replace('/account');
  };
  $('#enter-account').addEventListener('click', enter, { once: true });
  dialog.addEventListener(
    'cancel',
    (event) => {
      event.preventDefault();
      enter();
    },
    { once: true },
  );
  const reduced = matchMedia('(prefers-reduced-motion: reduce)').matches;
  if (!reduced) {
    const context = canvas.getContext('2d'),
      ratio = Math.min(devicePixelRatio || 1, 2),
      width = innerWidth,
      height = innerHeight;
    canvas.width = width * ratio;
    canvas.height = height * ratio;
    context.scale(ratio, ratio);
    const colors = ['#3b82f6', '#60a5fa', '#38bdf8', '#a5c9ff', '#66d5b0'];
    const particles = Array.from({ length: 64 }, () => ({
      x: width / 2,
      y: height / 2 - 35,
      vx: (Math.random() - 0.5) * 9,
      vy: -Math.random() * 7 - 2,
      angle: Math.random() * Math.PI,
      spin: (Math.random() - 0.5) * 0.2,
      width: Math.random() * 3 + 3,
      height: Math.random() * 5 + 4,
      color: colors[Math.floor(Math.random() * colors.length)],
    }));
    let previous, start;
    function draw(now) {
      start ??= now;
      const dt = Math.min((now - (previous ?? now)) / 16.67, 2);
      previous = now;
      context.clearRect(0, 0, width, height);
      context.globalAlpha = Math.max(0, 1 - (now - start) / 1800);
      for (const particle of particles) {
        particle.x += particle.vx * dt;
        particle.y += particle.vy * dt;
        particle.vy += 0.12 * dt;
        particle.angle += particle.spin * dt;
        context.save();
        context.translate(particle.x, particle.y);
        context.rotate(particle.angle);
        context.fillStyle = particle.color;
        context.fillRect(
          -particle.width / 2,
          -particle.height / 2,
          particle.width,
          particle.height,
        );
        context.restore();
      }
      if (now - start < 1800) frame = requestAnimationFrame(draw);
    }
    frame = requestAnimationFrame(draw);
  }
  navigation = setTimeout(enter, reduced ? 1200 : 2400);
  window.addEventListener(
    'pagehide',
    () => {
      clearTimeout(navigation);
      cancelAnimationFrame(frame);
    },
    { once: true },
  );
}
