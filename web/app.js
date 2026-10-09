'use strict';

const invoke = window.__TAURI__.core.invoke;
const listen = window.__TAURI__.event.listen;
const $ = (id) => document.getElementById(id);
const argumentMeasureContext = document.createElement('canvas').getContext('2d');

let state = null;
let toastTimer = null;
let checkingUpdate = false;
let downloadingUpdate = false;
const iconCache = new Map();
let defaultIcon = '';

function escapeHtml(value) {
  return String(value ?? '').replace(/[&<>"']/g, (char) => ({
    '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;',
  })[char]);
}

function currentConfig() {
  return JSON.stringify(state.config);
}

function isDirty() {
  return currentConfig() !== state.loadedConfig;
}

function defaultEntryName(path) {
  const fileName = String(path ?? '').split(/[\\/]/).pop() || '';
  const extension = fileName.lastIndexOf('.');
  return extension > 0 ? fileName.slice(0, extension) : fileName;
}

function showToast(message, kind = 'notice') {
  const toast = $('toast');
  toast.textContent = message;
  toast.className = `toast ${kind}`;
  toast.hidden = false;
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => { toast.hidden = true; }, 4200);
}

function updateFooter() {
  const dirty = isDirty();
  const button = $('startupButton');
  $('dirtyNote').hidden = !dirty;
  button.textContent = state.registered && !dirty ? '取消自启动' : state.registered ? '保存自启动配置' : '添加到开机启动';
  button.className = `button startup-button${state.registered && !dirty ? ' cancel' : state.registered ? ' enabled' : ''}`;
  $('headerState').textContent = dirty ? '有未保存的修改' : state.registered ? '已加入开机启动' : '配置已就绪';
}

function entryCard(entry, index) {
  const icon = iconCache.get(entry.iconPath || entry.path) || defaultIcon;
  const windows = entry.windows || [];
  const args = entry.args || [];
  return `<article class="entry-card${entry.enabled ? '' : ' is-disabled'}" data-index="${index}">
    <div class="entry-head">
      <button class="entry-app-icon" data-action="choose-icon" data-index="${index}" type="button" title="选择图标来源程序" aria-label="选择${escapeHtml(entry.name || '启动项')}的图标来源程序">${icon ? `<img src="${icon}" alt="">` : '<span aria-hidden="true">▣</span>'}</button>
      <input class="entry-name" data-field="name" data-index="${index}" value="${escapeHtml(entry.name || '')}" placeholder="新启动项" aria-label="启动项名称">
      <div class="entry-controls">
        <button class="entry-enabled-toggle${entry.enabled ? ' is-enabled' : ''}" data-action="toggle-enabled" data-index="${index}" type="button" role="switch" aria-checked="${entry.enabled}" title="${entry.enabled ? '点击禁用此启动项' : '点击启用此启动项'}"><span class="entry-toggle-track"><i></i></span><span>启用</span></button>
        <button class="button icon" data-action="up" data-index="${index}" title="上移" aria-label="上移">↑</button>
        <button class="button icon" data-action="down" data-index="${index}" title="下移" aria-label="下移">↓</button>
        <button class="button icon danger" data-action="remove" data-index="${index}" title="删除" aria-label="删除">×</button>
      </div>
    </div>
    <div class="entry-fields-row${windows.length ? ' has-window-detection' : ''}">
      <div class="compact-field path-field">
        <div class="path-controls">
          <input class="field-control" data-field="path" data-index="${index}" value="${escapeHtml(entry.path)}" placeholder="程序路径" aria-label="程序路径">
          <button class="button secondary" data-action="browse" data-index="${index}">浏览</button>
        </div>
      </div>
      <div class="compact-field window-field">
        <div class="window-chip-list">
          ${windows.map((title, valueIndex) => `<div class="window-chip">
            <input class="window-chip-input window-title-input" data-field="window-title" data-index="${index}" data-value-index="${valueIndex}" value="${escapeHtml(title)}" style="width:${contentWidth(title, '窗口检测')}px" placeholder="窗口检测" aria-label="窗口检测">
            <button class="button icon danger" data-action="remove-window-title" data-index="${index}" data-value-index="${valueIndex}" title="删除窗口检测" aria-label="删除窗口检测">×</button>
          </div>`).join('')}
          <input class="tag-entry-input" data-tag-input="window-title" data-index="${index}" placeholder="窗口检测" aria-label="输入窗口检测并按回车添加">
        </div>
      </div>
      <div class="compact-field args-field">
        <div class="arg-chip-list">
          ${args.map((arg, valueIndex) => `<div class="arg-chip">
            <input class="arg-chip-input" data-field="args" data-index="${index}" data-value-index="${valueIndex}" value="${escapeHtml(arg)}" style="width:${argumentWidth(arg)}px" placeholder="启动参数" aria-label="启动参数">
            <button class="button icon danger" data-action="remove-arg" data-index="${index}" data-value-index="${valueIndex}" title="删除启动参数" aria-label="删除启动参数">×</button>
          </div>`).join('')}
          <input class="tag-entry-input" data-tag-input="args" data-index="${index}" placeholder="启动参数" aria-label="输入启动参数并按回车添加">
        </div>
      </div>
      <div class="compact-field close-delay-field">
        <input class="field-control close-delay-input" type="text" inputmode="numeric" data-field="delay" data-index="${index}" value="${entry.delay || ''}" placeholder="延时(秒)" aria-label="延时秒数">
      </div>
    </div>
  </article>`;
}

function contentWidth(value, placeholder) {
  const text = String(value ?? '') || placeholder;
  argumentMeasureContext.font = getComputedStyle(document.body).font;
  return Math.max(16, Math.ceil(argumentMeasureContext.measureText(text).width + 4));
}

function argumentWidth(value) {
  return contentWidth(value, '启动参数');
}

function commitTagInput(target, restoreFocus = false) {
  const value = target.value.trim();
  if (!value) return false;
  const index = Number(target.dataset.index);
  const entry = state?.config.entries[index];
  if (!entry) return false;

  const type = target.dataset.tagInput;
  const values = type === 'window-title' ? entry.windows : entry.args;
  const valueIndex = values.length;
  values.push(value);
  const chip = type === 'window-title'
    ? `<div class="window-chip"><input class="window-chip-input window-title-input" data-field="window-title" data-index="${index}" data-value-index="${valueIndex}" value="${escapeHtml(value)}" style="width:${contentWidth(value, '窗口检测')}px" placeholder="窗口检测" aria-label="窗口检测"><button class="button icon danger" data-action="remove-window-title" data-index="${index}" data-value-index="${valueIndex}" title="删除窗口检测" aria-label="删除窗口检测">×</button></div>`
    : `<div class="arg-chip"><input class="arg-chip-input" data-field="args" data-index="${index}" data-value-index="${valueIndex}" value="${escapeHtml(value)}" style="width:${argumentWidth(value)}px" placeholder="启动参数" aria-label="启动参数"><button class="button icon danger" data-action="remove-arg" data-index="${index}" data-value-index="${valueIndex}" title="删除启动参数" aria-label="删除启动参数">×</button></div>`;
  target.insertAdjacentHTML('beforebegin', chip);
  target.value = '';
  updateFooter();
  if (restoreFocus) target.focus();
  return true;
}

document.addEventListener('keydown', (event) => {
  const target = event.target;
  if (event.key !== 'Enter' || event.isComposing || event.keyCode === 229 || !target.matches('[data-tag-input]')) return;
  if (!target.value.trim()) return;
  event.preventDefault();
  commitTagInput(target, true);
});

document.addEventListener('focusout', (event) => {
  if (event.target.matches('[data-tag-input]')) commitTagInput(event.target);
});

function renderEditor() {
  const editor = $('editorView');
  editor.innerHTML = state.config.entries.map(entryCard).join('');
  $('emptyState').hidden = state.config.entries.length !== 0;
  editor.hidden = false;
  $('editToolbar').hidden = false;
  $('editorActions').hidden = false;
  $('viewTitle').textContent = '启动顺序';
  $('viewDescription').textContent = '按顺序启动应用，并可等待指定窗口后继续。';
  updateFooter();
  $('networkTarget').value = state.config.networkTarget || '';
}

async function loadExecutableIcons() {
  const paths = [...new Set(state.config.entries.map((entry) => entry.iconPath || entry.path).filter(Boolean))];
  await Promise.all(paths.map(async (path) => {
    if (iconCache.has(path)) return;
    try {
      const icon = await invoke('executable_icon', { path });
      iconCache.set(path, icon || '');
    } catch { iconCache.set(path, ''); }
  }));
  renderEditor();
}

function setStatus(message, kind = 'ready') {
  $('statusText').textContent = message;
  const indicator = document.querySelector('.status-indicator');
  indicator.className = `status-indicator${kind === 'busy' ? ' busy' : kind === 'error' ? ' error' : ''}`;
}

async function initialize() {
  try {
    state = await invoke('get_state');
    $('appVersion').textContent = state.version || 'v0.1.0';
    $('appVersion').setAttribute('aria-label', `当前版本 ${$('appVersion').textContent}，点击检查更新`);
    try {
      const icon = await invoke('default_icon');
      if (icon) $('brandIcon').src = icon;
    } catch { /* Keep the header usable if the packaged icon cannot be read. */ }
    const launchedOnBoot = state.autostart;
    state.autostart = false;
    state.config.entries ||= [];
    state.config.networkTarget ??= '223.5.5.5';
    state.config.entries.forEach((entry) => {
      entry.name ||= '';
      entry.iconPath ||= '';
      entry.enabled ??= true;
      entry.delay = Math.min(86400, Math.max(0, Number.parseInt(entry.delay, 10) || 0));
      entry.windows = (Array.isArray(entry.windows) ? entry.windows : []).filter((title) => typeof title === 'string');
      entry.args ||= [];
    });
    defaultIcon = await invoke('default_icon');
    state.loadedConfig = JSON.stringify(state.config);
    renderEditor();
    if (launchedOnBoot) {
      try {
        await invoke('set_main_window_visible', { visible: false });
        await invoke('open_launch_progress', { config: state.config });
      } catch (error) {
        await invoke('set_main_window_visible', { visible: true });
        throw error;
      }
      return;
    }
    await invoke('set_main_window_visible', { visible: true });
    await loadExecutableIcons();
  } catch (error) {
    setStatus(String(error), 'error');
    showToast(String(error), 'error');
  }
}

document.addEventListener('input', (event) => {
  const target = event.target;
  if (target.id === 'networkTarget' && state && !state.autostart) {
    state.config.networkTarget = target.value.trim();
    updateFooter();
    return;
  }
  if (!target.matches('[data-field]') || !state || state.autostart) return;
  const index = Number(target.dataset.index);
  const valueIndex = Number(target.dataset.valueIndex);
  const entry = state.config.entries[index];
  if (!entry) return;
  switch (target.dataset.field) {
    case 'name': entry.name = target.value; break;
    case 'path': {
      const previousPath = entry.path;
      entry.path = target.value;
      if (!entry.name || entry.name === defaultEntryName(previousPath)) {
        entry.name = defaultEntryName(target.value);
        document.querySelector(`.entry-name[data-index="${index}"]`).value = entry.name;
      }
      break;
    }
    case 'window-title':
      entry.windows[valueIndex] = target.value;
      target.style.width = `${contentWidth(target.value, '窗口检测')}px`;
      break;
    case 'delay':
      entry.delay = Math.min(86400, Math.max(0, Number.parseInt(target.value, 10) || 0));
      break;
    case 'args':
      entry.args[valueIndex] = target.value;
      target.style.width = `${argumentWidth(target.value)}px`;
      break;
  }
  updateFooter();
});

document.addEventListener('change', (event) => {
  if (event.target.matches('[data-field="path"]') && state) loadExecutableIcons();
});

document.addEventListener('click', async (event) => {
  const button = event.target.closest('[data-action]');
  if (!button || !state || state.autostart) return;
  const index = Number(button.dataset.index);
  const action = button.dataset.action;
  switch (action) {
    case 'browse': {
      try {
        const path = await invoke('browse_executable');
        if (path) {
          const entry = state.config.entries[index];
          const previousPath = entry.path;
          entry.path = path;
          if (!entry.name || entry.name === defaultEntryName(previousPath)) {
            entry.name = defaultEntryName(path);
          }
          await loadExecutableIcons();
        }
      } catch (error) { showToast(String(error), 'error'); }
      break;
    }
    case 'choose-icon': {
      try {
        const path = await invoke('browse_executable');
        if (path) {
          state.config.entries[index].iconPath = path;
          await loadExecutableIcons();
        }
      } catch (error) { showToast(String(error), 'error'); }
      break;
    }
    case 'up':
      if (index > 0) [state.config.entries[index - 1], state.config.entries[index]] = [state.config.entries[index], state.config.entries[index - 1]];
      break;
    case 'down':
      if (index + 1 < state.config.entries.length) [state.config.entries[index + 1], state.config.entries[index]] = [state.config.entries[index], state.config.entries[index + 1]];
      break;
    case 'remove':
      state.config.entries.splice(index, 1);
      break;
    case 'toggle-enabled':
      state.config.entries[index].enabled = !state.config.entries[index].enabled;
      break;
    case 'remove-window-title':
      state.config.entries[index].windows.splice(Number(button.dataset.valueIndex), 1);
      break;
    case 'remove-arg':
      state.config.entries[index].args.splice(Number(button.dataset.valueIndex), 1);
      break;
    default: return;
  }
  renderEditor();
});

$('addButton').addEventListener('click', () => {
  if (!state) { showToast('配置尚未加载完成', 'error'); return; }
  state.config.entries.push({ name: '', path: '', iconPath: '', enabled: true, windows: [], delay: 0, args: [] });
  renderEditor();
  $('editorView').lastElementChild?.querySelector('[data-field="path"]')?.focus();
});
$('emptyAddButton').addEventListener('click', () => $('addButton').click());

$('startupButton').addEventListener('click', async () => {
  const enabled = !(state.registered && !isDirty());
  try {
    const result = await invoke('set_autostart', { config: state.config, enabled });
    state.registered = result.registered;
    if (enabled) state.loadedConfig = JSON.stringify(state.config);
    renderEditor();
    setStatus(result.message);
  } catch (error) {
    setStatus(String(error), 'error');
    showToast(String(error), 'error');
  }
});

$('runButton').addEventListener('click', async () => {
  try {
    await invoke('open_launch_progress', { config: state.config });
  } catch (error) { showToast(String(error), 'error'); }
});

$('closeButton').addEventListener('click', () => invoke('close_main_window'));

$('appVersion').addEventListener('click', async () => {
  if (checkingUpdate || downloadingUpdate) return;
  checkingUpdate = true;
  $('appVersion').disabled = true;
  setStatus('正在检查更新…', 'busy');
  try { await invoke('check_updates'); }
  catch (error) {
    checkingUpdate = false;
    $('appVersion').disabled = false;
    showUpdateDialog('检查更新失败', String(error), false);
    setStatus('检查更新失败', 'error');
  }
});

$('laterButton').addEventListener('click', () => { $('updateModal').hidden = true; });
$('updateModal').addEventListener('click', (event) => {
  if (event.target === $('updateModal') && !downloadingUpdate) $('updateModal').hidden = true;
});
$('updateButton').addEventListener('click', async () => {
  downloadingUpdate = true;
  $('appVersion').disabled = true;
  $('updateTitle').textContent = '正在下载更新';
  $('updateMessage').textContent = '下载完成后将替换当前程序并自动重启。';
  $('downloadProgress').hidden = false;
  $('downloadProgressFill').style.width = '0%';
  $('downloadProgressText').textContent = '0%';
  $('laterButton').hidden = true;
  $('updateButton').hidden = true;
  setStatus('正在下载更新…', 'busy');
  try { await invoke('install_update'); }
  catch (error) {
    downloadingUpdate = false;
    $('appVersion').disabled = false;
    showUpdateDialog('更新失败', String(error), false);
    setStatus('更新失败', 'error');
  }
});

function showUpdateDialog(title, message, hasUpdate) {
  $('updateTitle').textContent = title;
  $('updateMessage').textContent = message;
  $('downloadProgress').hidden = true;
  $('laterButton').hidden = false;
  $('laterButton').textContent = hasUpdate ? '稍后' : '关闭';
  $('updateButton').hidden = !hasUpdate;
  $('updateButton').textContent = '下载并重启';
  $('updateModal').hidden = false;
}

async function registerEvents() {
  await listen('update-status', ({ payload }) => {
    setStatus(payload, 'busy');
    if (downloadingUpdate) $('updateMessage').textContent = payload;
  });
  await listen('update-available', ({ payload }) => {
    checkingUpdate = false;
    $('appVersion').disabled = false;
    showUpdateDialog('发现新版本', `发现版本 ${payload.version}（当前版本 ${payload.currentVersion}）。下载后将替换程序并自动重启。`, true);
    setStatus(`发现新版本 ${payload.version}`, 'busy');
  });
  await listen('update-error', ({ payload }) => {
    const updateFailed = downloadingUpdate;
    checkingUpdate = false;
    downloadingUpdate = false;
    $('appVersion').disabled = false;
    showUpdateDialog(updateFailed ? '更新失败' : '检查更新失败', payload, false);
    setStatus(payload, 'error');
  });
  await listen('update-current', ({ payload }) => {
    checkingUpdate = false;
    $('appVersion').disabled = false;
    showUpdateDialog('没有可用更新', `当前已是最新版本（${state?.version || ''}）。`, false);
    setStatus(payload);
  });
  await listen('update-progress', ({ payload }) => {
    const percent = Math.max(0, Math.min(100, Number(payload.percent) || 0));
    $('downloadProgress').hidden = false;
    $('downloadProgressFill').style.width = `${percent}%`;
    $('downloadProgressText').textContent = `${percent}%`;
  });
}

registerEvents().catch((error) => {
  showToast(`界面事件初始化失败：${error}`, 'error');
}).finally(initialize);
