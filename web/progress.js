'use strict';

const invoke = window.__TAURI__.core.invoke;
const listen = window.__TAURI__.event.listen;
const $ = (id) => document.getElementById(id);
const icons = new Map();
const startedEntries = new Set();
let defaultIcon = '';

let config = { entries: [] };
let activeIndex = 0;
let networkReady = false;
let networkChecking = false;
let sequenceStarted = false;

function hasNetworkItem() {
  return Boolean(config.networkTarget?.trim());
}

function escapeHtml(value) {
  return String(value ?? '').replace(/[&<>"']/g, (char) => ({
    '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;',
  })[char]);
}

function entryName(entry) {
  if (entry.name?.trim()) return entry.name.trim();
  const fileName = entry.path?.split(/[\\/]/).pop() || '';
  const extension = fileName.lastIndexOf('.');
  return extension > 0 ? fileName.slice(0, extension) : fileName || '启动项';
}

function iconMarkup(path) {
  const icon = icons.get(path);
  if (icon) return `<img src="${icon}" alt="">`;
  return `<img src="${defaultIcon}" alt="">`;
}

function renderTrack() {
  const networkOffset = hasNetworkItem() ? 1 : 0;
  const displayEntries = [
    ...(networkOffset ? [{ kind: 'network', name: '网络检测', path: '' }] : []),
    ...config.entries.map((entry, index) => ({ kind: 'entry', entry, index })),
  ];
  const activeDisplayIndex = networkChecking ? 0 : activeIndex + networkOffset;
  const slots = [];
  for (let offset = -2; offset <= 2; offset += 1) {
    const index = activeDisplayIndex + offset;
    if (index < 0 || index >= displayEntries.length) {
      slots.push('<span class="launch-slot empty" aria-hidden="true"></span>');
      continue;
    }
    const item = displayEntries[index];
    if (!item) {
      slots.push('<span class="launch-slot empty" aria-hidden="true"></span>');
      continue;
    }
    const isNetworkItem = item.kind === 'network';
    const entryIndex = isNetworkItem ? -1 : item.index;
    const entry = item.entry;
    const name = isNetworkItem ? item.name : entryName(entry);
    const current = isNetworkItem ? networkChecking : sequenceStarted && entryIndex === activeIndex;
    const started = isNetworkItem ? networkReady && !networkChecking : startedEntries.has(entryIndex) && !current;
    const pending = !current && !started;
    const className = `launch-step${current ? ' current' : started ? ' started' : ' pending'}`;
    const caption = `<span class="launch-name">${escapeHtml(name)}</span>`;
    const icon = `<span class="launch-icon">${iconMarkup(isNetworkItem ? '' : entry.iconPath || entry.path)}</span>${caption}`;
    if (pending && !isNetworkItem) {
      slots.push(`<button class="launch-step launch-slot ${className}" type="button" data-launch-index="${entryIndex}" aria-label="跳过当前等待并启动${escapeHtml(name)}" title="跳过当前等待并启动此项"${networkReady ? '' : ' disabled'}>${icon}</button>`);
      continue;
    }
    slots.push(`<div class="launch-step launch-slot ${className}" aria-current="${current ? 'step' : 'false'}">${icon}</div>`);
  }
  $('launchTrack').innerHTML = slots.join('');
}

function setMessage(message, kind = '') {
  const element = $('progressMessage');
  element.textContent = message;
  element.dataset.state = kind;
}

function updateProgress(payload) {
  $('progressWindow').textContent = payload.windowTitle || '';
  const seconds = payload.remainingSeconds;
  $('progressCountdown').textContent = seconds == null ? '' : `${seconds} 秒`;
  if (payload.status?.startsWith('启动失败') || payload.status?.startsWith('跳过：') || payload.status?.startsWith('关闭窗口失败')) {
    setMessage(payload.status, payload.status.startsWith('启动失败') || payload.status.startsWith('关闭窗口失败') ? 'error' : '');
  } else {
    setMessage('');
  }
}

async function loadIcons() {
  await Promise.all(config.entries.map(async (entry) => {
    const iconPath = entry.iconPath || entry.path;
    if (!iconPath || icons.has(iconPath)) return;
    try {
      icons.set(iconPath, await invoke('executable_icon', { path: iconPath }) || '');
    } catch {
      icons.set(iconPath, '');
    }
  }));
  renderTrack();
}

async function registerEvents() {
  await listen('network-progress', () => {
    networkReady = false;
    networkChecking = true;
    sequenceStarted = false;
    activeIndex = 0;
    renderTrack();
    $('progressWindow').textContent = '';
    $('progressCountdown').textContent = '';
    setMessage('');
  });
  await listen('network-ready', () => {
    networkReady = true;
    networkChecking = false;
    sequenceStarted = config.entries.length > 0;
    activeIndex = 0;
    $('progressWindow').textContent = '';
    $('progressCountdown').textContent = '';
    renderTrack();
  });
  await listen('launch-progress', ({ payload }) => {
    activeIndex = payload.index;
    sequenceStarted = true;
    if (payload.started) startedEntries.add(payload.index);
    renderTrack();
    updateProgress(payload);
  });
  await listen('launch-finished', () => {
    invoke('finish_launch_progress');
  });
  await listen('update-status', ({ payload }) => setMessage(payload));
  await listen('update-current', ({ payload }) => setMessage(payload));
  await listen('update-error', ({ payload }) => setMessage(payload, 'error'));
  await listen('update-available', ({ payload }) => {
    $('updateMessage').textContent = `发现版本 ${payload.version}（当前版本 ${payload.currentVersion}）。下载后将替换程序并自动重启。`;
    $('updateModal').hidden = false;
    setMessage(`发现新版本 ${payload.version}`);
  });
}

async function initialize() {
  try {
    await registerEvents();
    const request = await invoke('take_launch_request');
    if (!request) throw new Error('没有待执行的启动请求');
    config = request.config || { entries: [] };
    config.entries ||= [];
    config.networkTarget ||= '';
    defaultIcon = await invoke('default_icon');
    networkChecking = hasNetworkItem();
    sequenceStarted = !networkChecking && config.entries.length > 0;
    renderTrack();
    setMessage('');
    await loadIcons();
    await invoke('run_sequence', { config });
  } catch (error) {
    setMessage(String(error), 'error');
  }
}

$('launchTrack').addEventListener('click', async (event) => {
  const button = event.target.closest('[data-launch-index]');
  if (!button || !networkReady) return;
  const index = Number(button.dataset.launchIndex);
  const previousIndex = activeIndex;
  activeIndex = index;
  sequenceStarted = true;
  $('progressWindow').textContent = '';
  $('progressCountdown').textContent = '';
  renderTrack();
  try {
    await invoke('jump_to_launch', { index });
    setMessage('正在跳过当前等待…');
  } catch (error) {
    activeIndex = previousIndex;
    renderTrack();
    setMessage(String(error), 'error');
  }
});

$('closeProgressButton').addEventListener('click', () => invoke('close_launch_progress'));
$('settingsButton').addEventListener('click', async () => {
  try {
    await invoke('dismiss_launch_progress');
  } catch (error) {
    setMessage(String(error), 'error');
  }
});
$('laterButton').addEventListener('click', () => {
  $('updateModal').hidden = true;
});
$('updateModal').addEventListener('click', (event) => {
  if (event.target === $('updateModal')) $('updateModal').hidden = true;
});
$('updateButton').addEventListener('click', async () => {
  $('updateModal').hidden = true;
  setMessage('正在下载更新，完成后会自动重启…');
  try {
    await invoke('install_update');
  } catch (error) {
    setMessage(String(error), 'error');
  }
});

initialize();
