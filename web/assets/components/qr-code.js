let library;

function loadQRCode() {
  if (window.QRCode) return Promise.resolve(window.QRCode);
  library ??= new Promise((resolve, reject) => {
    const script = document.createElement('script');
    script.src = new URL('../qrcode.js', import.meta.url).href;
    script.onload = () => {
      if (window.QRCode) resolve(window.QRCode);
      else reject(new Error('二维码暂时无法显示，请刷新后重试。'));
    };
    script.onerror = () => reject(new Error('二维码暂时无法显示，请刷新后重试。'));
    document.head.append(script);
  }).catch((error) => {
    library = undefined;
    throw error;
  });
  return library;
}

export async function drawQR(container, content, signal) {
  if (!content) throw new Error('二维码暂时无法显示，请刷新后重试。');
  const QRCode = await loadQRCode();
  const source = await QRCode.toDataURL(content, {
    width: 440,
    margin: 2,
    errorCorrectionLevel: 'M',
    color: { dark: '#172641', light: '#ffffff' },
  });
  // QR generation can finish after a login method/session has changed.
  signal?.throwIfAborted();
  const image = new Image();
  image.alt = '微信扫码二维码';
  image.src = source;
  container.replaceChildren(image);
}
