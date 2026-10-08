// Highlights the download for the visitor's OS, shows the latest version,
// and wires the copy buttons. The page works without any of this.

const REPO = 'adeebahmad01/clipcut';

function detectTarget() {
  const ua = navigator.userAgent;
  const platform = (navigator.userAgentData && navigator.userAgentData.platform) || navigator.platform || '';
  if (/Win/i.test(platform) || /Windows/i.test(ua)) return { label: 'Windows (WSL2)', target: null, win: true };
  if (/Mac/i.test(platform) || /Mac OS X/i.test(ua)) {
    // Browsers report Intel on every Mac, so default to the common case.
    return { label: 'macOS', target: 'aarch64-apple-darwin' };
  }
  if (/Linux/i.test(platform) || /Linux/i.test(ua)) {
    const arm = /aarch64|arm64|armv8/i.test(platform + ' ' + ua);
    return { label: arm ? 'Linux ARM64' : 'Linux', target: arm ? 'aarch64-unknown-linux-gnu' : 'x86_64-unknown-linux-gnu' };
  }
  return null;
}

const found = detectTarget();
if (found) {
  document.getElementById('hero-os').textContent = found.label;
  const card = found.win
    ? document.querySelector('.plat.win')
    : document.querySelector(`.plat[data-target="${found.target}"]`);
  if (card) card.classList.add('mine');
}

document.querySelectorAll('[data-copy]').forEach((btn) => {
  btn.addEventListener('click', async () => {
    const text = document.getElementById(btn.dataset.copy).textContent;
    try {
      await navigator.clipboard.writeText(text);
      btn.textContent = 'Copied';
    } catch {
      btn.textContent = 'Select and copy';
    }
    setTimeout(() => { btn.textContent = 'Copy'; }, 1800);
  });
});

fetch(`https://api.github.com/repos/${REPO}/releases/latest`)
  .then((r) => (r.ok ? r.json() : null))
  .then((rel) => {
    if (!rel || !rel.tag_name) return;
    const line = document.getElementById('version-line');
    const link = document.createElement('a');
    link.href = `https://github.com/${REPO}/releases/latest`;
    link.textContent = rel.tag_name;
    line.replaceChildren('Latest: ', link, ' · Linux and macOS · Windows via WSL2 · MIT license');
  })
  .catch(() => {});
