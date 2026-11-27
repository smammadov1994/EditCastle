const byId = id => document.getElementById(id);
const preview = byId('preview');
const timelineBody = byId('timeline-body');
const librarySkimPlayer = document.createElement('video');
librarySkimPlayer.muted = librarySkimPlayer.playsInline = true;
librarySkimPlayer.preload = 'auto';
const librarySkimFrame = byId('source-skim-frame');
const librarySkimLabel = byId('source-skim-label');
let librarySkim;
const soundtrack = new Audio();
soundtrack.preload = 'metadata';
let shortcutStorage;
try { shortcutStorage = window.localStorage; } catch (_) {}
const shortcutPreferences = new StudioShortcuts.Preferences(shortcutStorage);
const editorThemes = ['pro','palenight','clay','auto','dark','grey','amber','light'];
function displayPreference(name,fallback) {
  try { return localStorage.getItem('editroom.'+name) ?? fallback; } catch (_) { return fallback; }
}
function storeDisplayPreference(name,value) {
  try { localStorage.setItem('editroom.'+name,value); } catch (_) {}
}
let editorTheme=displayPreference('appearance','pro');
// Pro is the studio's look now. Move anyone still on the old default over once; a later choice sticks.
if (displayPreference('appearance-pro','')!=='seen') {
  if (editorTheme==='palenight') editorTheme='pro';
  storeDisplayPreference('appearance','pro'===editorTheme ? 'pro' : editorTheme);
  storeDisplayPreference('appearance-pro','seen');
}
if (!editorThemes.includes(editorTheme)) editorTheme='pro';
let timeStyle=displayPreference('time-style','timecode') === 'simple' ? 'simple' : 'timecode';
let directorEnabled=displayPreference('director-enabled','true') === 'true';
let directorPanelOpen=displayPreference('director-open','true') === 'true';
let showDirectorWork=displayPreference('director-work','true') === 'true';
const systemAppearance=matchMedia('(prefers-color-scheme: dark)');
function applyEditorTheme() {
  document.documentElement.dataset.editorTheme=editorTheme==='auto' ? (systemAppearance.matches ? 'dark' : 'light') : editorTheme;
}
applyEditorTheme();
systemAppearance.addEventListener('change',applyEditorTheme);
let state;
let stateRequested=0;
let stateReceived=0;
let projectId = sessionStorage.getItem('editroom.project');
let selected;
const selection = new Set();
let clipClipboard;
let project;
let playingCut = false;
let waitingForAudio;
let playbackStart = 0;
let playbackOrigin = 0;
let playbackFrame;
let editMode;
let submittingExport = false;
let submittingDirector = false;
const TICKS_PER_SECOND = 240000;
const ticks = seconds => Math.round(Number(seconds) * TICKS_PER_SECOND);
const frameStep = () => project?.frame_step ?? TICKS_PER_SECOND / 24;
const onFrame = time => Math.round(time / frameStep()) * frameStep();
const length = clip => clip.duration;
const clipTrack = clip => clip.track || (clip.kind === 'audio' ? 'A1' : 'V1');
const clipTitle = clip => clip.kind === 'text' ? clip.effects.title : clip.kind === 'solid' ? 'Solid' : source(clip.source)?.title || 'Offline media';
let placement = new Map();
function locateTimeline(project) {
  const endings = new Map();
  const positions = new Map();
  for (const clip of project.timeline) {
    const track = clipTrack(clip);
    const position = clip.position ?? endings.get(track) ?? 0;
    positions.set(clip.id, position);
    endings.set(track, Math.max(endings.get(track) ?? 0, position + length(clip)));
  }
  return positions;
}
const clipTotal = () => Math.max(0, ...project.timeline.map(clip => timelineStart(clip.id) + length(clip)));
const timelineStart = id => placement.get(id) || 0;
const picture = clip => !clip.kind || clip.kind === 'video' || clip.kind === 'image';
const basePictures = () => project.timeline.filter(clip => picture(clip) &&
  !source(clip.source)?.missing_file &&
  !project.sequence.tracks.find(track => track.name === clipTrack(clip))?.hidden)
  .sort((a,b) => timelineStart(a.id) - timelineStart(b.id) || clipTrack(a).localeCompare(clipTrack(b)));
function clockTime(at, decimals) {
  const divisor=decimals ? 100 : 1;
  const units=Math.round(Math.max(0,at)/TICKS_PER_SECOND*divisor);
  const seconds=Math.floor(units/divisor);
  const hours=Math.floor(seconds/3600), minutes=Math.floor(seconds/60)%60;
  const pair=value=>String(value).padStart(2,'0');
  return (hours ? hours+':'+pair(minutes) : minutes) + ':'+pair(seconds%60) + (decimals ? '.'+pair(units%100) : '');
}
const tc = time => {
  if (timeStyle==='simple') return clockTime(time,true);
  const rate = Math.round(project?.sequence.fps ?? 24);
  const frame = Math.round(Math.max(0, time) / frameStep());
  return [Math.floor(frame / (rate * 3600)), Math.floor(frame / (rate * 60)) % 60,
    Math.floor(frame / rate) % 60, frame % rate].map(n => String(n).padStart(2, '0')).join(':');
};
const fromTc = value => {
  const text = value.trim();
  if (timeStyle==='simple' && /^\d+(?::\d{1,2}){0,2}(?:\.\d+)?$/.test(text)) {
    return ticks(text.split(':').reduce((elapsed,part)=>elapsed*60+Number(part),0));
  }
  const parts = /[:;.]/.test(text) ? text.split(/[:;.]+/).filter(Boolean)
    : /^\d{1,8}$/.test(text) ? text.padStart(Math.ceil(text.length / 2) * 2, '0').match(/../g) : [];
  if (!parts.length || parts.length > 4 || !parts.every(part => /^\d{1,8}$/.test(part))) return null;
  const [frame, second = 0, minute = 0, hour = 0] = parts.map(Number).reverse();
  return (((hour * 60 + minute) * 60 + second) * Math.round(project?.sequence.fps ?? 24) + frame) * frameStep();
};
let pxPerTick = 80 / TICKS_PER_SECOND;
const maxTimelineScale = 2400 / TICKS_PER_SECOND;
let timelineRows;
let playLoop = false;
let editorTool = 'select';
let quick = false;
let sourcePreview;
let librarySearch;
let libraryFolder;
let libraryAnchor;
const librarySelection = new Set();
const libraryFolderSelection = new Map();
let draggedSources;
let mediaDropOverlay;
let pendingMediaDrop;
const sourceDragType = 'application/x-editroom-source';
const graphicDrags = [
  {button:'new-title',type:'application/x-editroom-title',tool:'add_title',label:'Text',fields:{text:'Text'}},
  {button:'new-solid',type:'application/x-editroom-solid',tool:'add_solid',label:'Solid',fields:{}},
];
let locateTarget;
let folderMenuScope;
let pendingLibraryRemoval;
let planningLibraryRemoval = false;
let sourceToReload;
const librarySort = new Intl.Collator(undefined,{numeric:true,sensitivity:'base'});
let searchTimer;
let searchAbort;
let shownRun;
let queuedReplay;
let fitCompletedDirector=false;
let savedProject;
let directorWatch;
let directorFrame;
let directorAnimation;
let presentedFrame;
let previewSelection;
let stoppingRun;
const previewedRuns = new Set();
let captions = [];
const captionSelection = new Set();
let captionFocus = false;
let captionEdit;
let captionStyle = {};
let captionWrites = Promise.resolve();
let pictureDraft;
let copiedGrade;
let pictureWrites = Promise.resolve();
const overlayPreviews = new Map();
const maxOverlayPreviewBytes = 32*1024*1024;
const maxOverlayPreviews = 48;
let overlayPreviewBytes = 0;
let overlayPreviewPending = 0;
const gradeCache = new Map();
const gradePending = new Set();
let draftGradeRequest;
let queuedGradeDraft;
let gradeDraftTimer;

let captionLayoutKey;
let frameAt = 0;
let skimFrame = null;
let markerPanel;
let markerDraft;
let rangeDraft;
let markerRenameTimer;
let markerWrites = Promise.resolve();
let pictureLayers = new Map();
let audioLayers = new Map();
let audioContext;
let masterBus;
const trackBuses = new Map();
const playerSignals = new WeakMap();
const meterReadings = new WeakMap();
let duckMix = {};
let mixRequest;
let duckTrack;
let trackGainDraft;
let trackGainWrites=Promise.resolve();
let scrubTimer;
let lastScrub = 0;
let viewerZoom = null;
let retimeProjection;
const viewerPanel=byId('viewer-panel');
const viewerStage=byId('viewer-stage');
const viewerViewport=byId('viewer-viewport');
const viewerPicture=byId('viewer-picture');
const compositeCanvas = byId('composite');
const paint = compositeCanvas.getContext('2d',{alpha:false});
const filmstripFrames = new StudioFilmstrip.Frames(scheduleTimelineMedia);
const gradedFilmstripFrames = new WeakMap();
const audioWaveforms = new Map();
let loadingWaveforms = 0;
let timelineMediaPaint;
const time = ticks => (ticks / TICKS_PER_SECOND).toFixed(2);
const sourceUrl = (route,id,version = source(id)?.version) => {
  const params=new URLSearchParams();
  if (version) params.set('v',version);
  if (presentedFrame && !savedProject.sources.some(item=>item.id===id && item.version===version)) {
    params.set('draft',presentedFrame.run);
  }
  return '/api/' + route + '/' + id + (params.size?'?'+params:'');
};
const mediaUrl = id => sourceUrl('media',id);
const previewQuery = () => presentedFrame ? '?draft='+presentedFrame.run+'&frame='+presentedFrame.sequence : '';
const clipAudioUrl = clip => clip.timed_audio
  ? projectUrl('/audio-preview/' + clip.id + '/' + clip.timed_audio + previewQuery())
  : mediaUrl(clip.source);
const projectUrl = (suffix = '') => '/api/projects/' + projectId + suffix;
const option = (title, value) => new Option(title, value);
const status = message => { byId('edit-status').textContent = message; };
const titleColor = fx => fx.transform?.color ?? (fx.color === '#272733' ? '#ffffff' : fx.color);
function item(tag, className, text) {
  const el = document.createElement(tag);
  el.className = className;
  el.textContent = text;
  return el;
}

async function api(url, { method = 'GET', data, file, raw = false, signal, headers:extra } = {}) {
  const headers = {...extra};
  let body;
  if (data !== undefined) {
    headers['content-type'] = 'application/json';
    body = JSON.stringify(data);
  } else if (file) {
    headers['x-editroom-title'] = file.name.replace(/[^ -~]/g, '_').slice(0, 100);
    headers['content-type'] = 'application/octet-stream';
    body = file;
  }
  const response = await fetch(url, { method, headers, body, cache: 'no-store', signal });
  if (!response.ok) {
    const err = await response.json().catch(() => ({}));
    if (response.status === 401 && url !== '/api/login') showLogin();
    const error = new Error(err.error || 'The studio returned HTTP ' + response.status + '.');
    error.status = response.status;
    throw error;
  }
  if (raw === 'response') return response;
  if (raw) return response.blob();
  return response.status === 204 ? undefined : response.json();
}
const post = (url, data) => api(url, { method: 'POST', data });
const source = id => project.sources.find(item => item.id === id);
const currentClip = () => project?.timeline.find(clip => clip.id === selected);

function showLogin() {
  elapsedRun=null;
  clearLibrarySkim(true);
  clearRetimeProjection();
  closeTimelineMenu();
  exitViewerFullScreen();
  filmstripFrames.reset();
  audioWaveforms.clear();
  clearOverlayPreviews();
  directorAnimation?.abort();
  closeDirectorPreview();
  closePlaybackWatch();
  closeMcpWatch();
  byId('sequence-offer').close();
  byId('folder-remove-dialog').close();
  exportDialog.close();
  byId('provider-dialog').close();
  byId('editor-settings-dialog').close();
  byId('revert-project-dialog').close();
  byId('save-as-dialog').close();
  byId('recent-projects-dialog').close();
  byId('project-title').textContent='';
  byId('project-unsaved').hidden=true;
  exportQueueVersion++;
  updateExportQueue([]);
  pendingExportFiles.clear();
  pendingMediaDrop = null;
  clearMediaDropOverlay();
  pauseTimeline();
  preview.pause();
  soundtrack.pause();
  byId('studio').hidden = byId('studio-actions').hidden = true;
  byId('login').hidden = false;
}

async function refresh(openProjectId) {
  const request=++stateRequested;
  const response=await api('/api/state',{raw:'response'});
  const receivedAt=performance.now();
  const sampledAt=Date.parse(response.headers.get('date') || '');
  const next=await response.json();
  if (request<stateReceived) {
    if (openProjectId) {
      projectId=openProjectId;
      renderStudio(state);
    }
    return;
  }
  stateReceived=request;
  if (!studioClock && Number.isFinite(sampledAt)) studioClock={sampledAt,receivedAt};
  if (openProjectId) projectId=openProjectId;
  renderStudio(next);
}

let playbackWatch = null;

function closePlaybackWatch() {
  playbackWatch?.controller.abort();
  playbackWatch = null;
}

function observePlayback() {
  if (playbackWatch?.project === projectId) return;
  closePlaybackWatch();
  const watch = {project:projectId, origin:crypto.randomUUID(), controller:new AbortController(), version:-1,
    pending:null, writes:0, queue:Promise.resolve()};
  playbackWatch = watch;
  readPlayback(watch);
}

async function readPlayback(watch) {
  const {signal} = watch.controller;
  while (!signal.aborted) {
    const cursor = watch.version < 0 ? '' : `?after=${watch.version}`;
    try {
      const start=performance.now();
      const update = await api(`/api/projects/${watch.project}/playback${cursor}`, {signal});
      if (!cursor && update) {
        watch.clockOffset=update.sampled_at_ms-performance.timeOrigin-(start+performance.now())/2;
      }
      receivePlayback(watch,update);
    } catch (error) {
      if (signal.aborted) return;
      // The ordinary project refresh can reconnect a watcher dropped by the network.
      if (playbackWatch===watch) closePlaybackWatch();
      return;
    }
  }
}

function receivePlayback(watch,update) {
  if (!update || playbackWatch!==watch || update.version<=watch.version) return;
  watch.version=update.version;
  watch.pending=update.origin===watch.origin ? null : update;
  applyPlayback();
}

function applyPlayback() {
  const watch=playbackWatch;
  if (watch?.project!==projectId || !watch.pending || watch.writes || presentedFrame || directorOwnsCut()) return;
  if (watch.pending.cut_revision>project.revision) {
    if (!watch.refreshing) {
      watch.refreshing=true;
      refresh().catch(()=>{}).finally(()=>{watch.refreshing=false;});
    }
    return;
  }
  const update=watch.pending;
  watch.pending=null;
  playLoop=update.looping;
  byId('loop').setAttribute('aria-pressed',String(playLoop));
  const sample=()=>samplePlayback(watch,update);
  const {at,playing}=sample();
  seekTimeline(at,false,false);
  if (playing) playTimeline(false,sample);
}

function samplePlayback(watch,update) {
  if (!update.playing) return update;
  const [first,last]=update.looping ? playRange() : [0,clipTotal()];
  const elapsed=watch.clockOffset===undefined ? 0 : Math.max(0,
    (performance.timeOrigin+performance.now()+watch.clockOffset-update.sampled_at_ms)/1000);
  let at=update.at+Math.round(elapsed*TICKS_PER_SECOND);
  if (at>=last) {
    if (!update.looping || last<=first) return {at:last,playing:false};
    at=first+(at-last)%(last-first);
  }
  return {at:Math.max(first,Math.floor(at/frameStep())*frameStep()),playing:true};
}

function sendPlayback(action) {
  const watch=playbackWatch;
  if (watch?.project!==projectId || presentedFrame || directorOwnsCut()) return;
  const command={action,at:frameAt,looping:playLoop,origin:watch.origin};
  watch.writes++;
  watch.queue=watch.queue.then(async()=>{
    try {
      if (playbackWatch===watch) receivePlayback(watch,await post(`/api/projects/${watch.project}/playback`,command));
    } catch (error) {
      if (playbackWatch===watch) status(error.message);
    } finally {
      watch.writes--;
      if (playbackWatch===watch) applyPlayback();
    }
  });
}

function renderStudio(nextState) {
  state = nextState;
  const wasHidden = byId('studio').hidden;
  byId('studio').hidden = byId('studio-actions').hidden = false;
  byId('login').hidden = true;
  if (wasHidden) refreshExportQueue();
  const saved = state.projects.find(item => item.id === projectId) || state.projects[0];
  const run=state.runs.find(run=>run.project===saved.id);
  const direct = showDirectorWork && directorFrame && directorFrame.run===run?.id && run.status==='running' &&
    !run.stop_requested && stoppingRun!==run.id && directorFrame.base_revision===saved.revision ? directorFrame : null;
  const external = mcpFrame?.project===saved.id && mcpFrame.base_revision===saved.revision ? mcpFrame : null;
  const frame=direct || external;
  const openingDraft=frame && !presentedFrame;
  const leavingDraft=!frame && presentedFrame;
  if (openingDraft) previewSelection={id:saved.id,selected,ids:[...selection],at:frameAt,
    scrollX:timelineBody.scrollLeft,scrollY:timelineBody.scrollTop,
    library:{assets:[...librarySelection],folders:[...libraryFolderSelection],anchor:libraryAnchor,
      location:libraryFolder && {...libraryFolder},source:sourcePreview?.id,time:preview.currentTime}};
  const previousView=leavingDraft && previewSelection?.id===saved.id ? previewSelection : null;
  const restoreView=previousView && (presentedFrame.external
    ? !presentedFrame.committed && saved.revision===presentedFrame.base_revision
    : (!showDirectorWork && saved.revision===presentedFrame.base_revision) ||
      (!run?.applied && (run?.stop_requested || run?.status==='failed' || stoppingRun===run?.id || presentedFrame.base_revision!==saved.revision)));
  if (previousView) {
    selected=saved.timeline.some(clip=>clip.id===previousView.selected) ? previousView.selected : null;
    selection.clear();
    previousView.ids.filter(id=>saved.timeline.some(clip=>clip.id===id)).forEach(id=>selection.add(id));
  }
  if (restoreView) {
    clearLibrarySelection();
    const {assets,folders,anchor,location} = previousView.library;
    assets.forEach(id=>librarySelection.add(id));
    folders.forEach(([key,pick])=>libraryFolderSelection.set(key,pick));
    libraryAnchor=anchor;
    libraryFolder=location;
  }
  if (leavingDraft) previewSelection=null;
  const previousFrame=presentedFrame;
  presentedFrame=frame;
  if (direct) {
    previewedRuns.add(run.id);
    if (previewedRuns.size>30) previewedRuns.delete(previewedRuns.values().next().value);
  }
  savedProject=saved;
  const next=frame ? {...saved,...frame.scene,messages:saved.messages,canUndo:false,canRedo:false} : saved;
  const projectChanged = project?.id !== next.id;
  const formatChanged=projectChanged || next.sequence.width!==project.sequence.width || next.sequence.height!==project.sequence.height;
  if (!projectChanged && next.sequence.tracks.some(track=>project.sequence.tracks.find(old=>old.name===track.name)?.id!==track.id)) duckMix={};
  const shouldRender = frame!==previousFrame || projectChanged || next.revision !== project?.revision || next.library_revision !== project?.library_revision || next.sources.length !== project?.sources.length;
  if (shouldRender && retimeProjection) clearRetimeProjection();
  project = next;
  projectId = next.id;
  if (projectChanged || wasHidden) restorePromptDraft();
  if (formatChanged) layoutViewer(true);
  const editPane=document.querySelector('.editing');
  const locked=directorOwnsCut();
  if (locked!==editPane.inert || locked && projectChanged) {
    pauseTimeline();
    preview.pause();
  }
  byId('project-title').textContent=next.title;
  byId('project-unsaved').hidden=!saved.has_unsaved_changes;
  byId('file-save').disabled=locked || Boolean(frame);
  byId('file-save-as').disabled=locked || Boolean(frame);
  byId('file-revert').disabled=locked || Boolean(frame) || !saved.has_unsaved_changes;
  byId('clip-commands').disabled=byId('mark-commands').disabled=locked || Boolean(frame);
  if (locked || openingDraft || projectChanged) closeTimelineMenu();
  if (!autosaveSetting.disabled) autosaveSetting.checked=state.autosave;
  if (locked && !editPane.inert) clearLibrarySkim();
  editPane.inert=locked;
  timelineBody.dataset.draft=String(Boolean(frame));
  byId('draft-label').hidden=!frame;
  byId('draft-label').textContent=external && frame===external ? 'Attached agent preview' : 'Director preview';
  if (projectChanged) {
    directorAnimation?.abort();
    clearLibrarySkim(true);
    filmstripFrames.reset();
    clearOverlayPreviews();
    if (exportDialog.open) exportDialog.close();
    libraryFolder = null;
    clearLibrarySelection();
    byId('folder-remove-dialog').close();
    draggedSources = null;
    pendingMediaDrop = null;
    clearMediaDropOverlay();
    resetLayers();
    duckMix = {};
    trackGainDraft = null;
    captions = [];
    captionSelection.clear();
    captionFocus = false;
    captionEdit = null;
    captionStyle = {};
    pictureDraft = null;
    queuedGradeDraft = null;
    captionLayoutKey = null;
    selected = undefined;
    selection.clear();
    pauseTimeline();
    preview.pause();
    sourcePreview = null;
    byId('audio-preview-name').hidden = true;
    frameAt = project.sequence.playhead || 0;
    byId('timecode').value=tc(frameAt);
    clearSkim(false);
    closeMarkerPanel(true);
    markerDraft = null;
    rangeDraft = null;
  }
  sessionStorage.setItem('editroom.project', projectId);
  if (projectChanged || wasHidden) noteRecentProject(projectId);
  renderProjectPicker();
  if (shouldRender) {
    if (waitingForAudio) pauseTimeline();
    if (sourcePreview) {
      const latest = source(sourcePreview.id);
      if (!latest) leaveSourcePreview();
      else if (latest.version !== sourcePreview.version) previewSource(latest, Math.min(preview.currentTime, Math.max(0,latest.duration / TICKS_PER_SECOND - .01)));
    }
    renderProject(projectChanged);
    refreshMix();
    loadCaptions();
    if (exportDialog.open) refreshExportSettings();

  }
  updateExportAvailability();
  if (libraryQuery() && !searchAbort && (shouldRender || !searchForCurrentProject() || librarySearch.data?.indexing)) queueLibrarySearch(0);
  if (restoreView) {
    seekTimeline(previousView.at,false,false);
    timelineBody.scrollLeft=previousView.scrollX;
    timelineBody.scrollTop=previousView.scrollY;
    const asset=source(previousView.library.source);
    if (asset) previewSource(asset,previousView.library.time);
  }
  observePlayback();
  applyPlayback();
  renderActivity();
  observeDirectorDraft(run);
  observeMcp(saved.id);
  if (queuedReplay) { const steps = queuedReplay; queuedReplay = null; performDirectorEdits(steps); }
  if (fitCompletedDirector) { fitCompletedDirector=false; zoomTimeline(null); }
  renderCredits();
  renderSequenceOffer(saved, run?.status === 'running');
  finishMediaDrop();
}

function renderProjectPicker() {
  const picker = byId('project-select');
  const projects = JSON.stringify(state.projects.map(entry => [entry.id, entry.title]));
  if (picker.dataset.projects !== projects) {
    picker.dataset.projects = projects;
    picker.replaceChildren(...state.projects.map(entry => option(entry.title, entry.id)));
  }
  picker.value = projectId;
}

function directorOwnsCut(editor=projectId) {
  return Boolean(state?.runs.some(run=>run.project===editor && run.status==='running' &&
    !run.applied && !run.stop_requested && run.id!==stoppingRun));
}

function updateExportAvailability() {
  byId('export').disabled=!project?.timeline.length || submittingExport || Boolean(presentedFrame) || directorOwnsCut();
}

function renderProject(fitTracks = false) {
  placement = locateTimeline(project);
  const seq = project.sequence;
  byId('sequence-width').value = seq.width;
  byId('sequence-height').value = seq.height;
  byId('sequence-fps').value = seq.fps;
  const sizeSelect = byId('sequence-size');
  const size = seq.width + ' × ' + seq.height;
  sizeSelect.querySelector('[data-current]')?.remove();
  if (![...sizeSelect.options].some(entry=>entry.value===size)) {
    const current = option(size,size);
    current.dataset.current = '';
    sizeSelect.append(current);
  }
  sizeSelect.value = size;
  for (const [id,player] of pictureLayers) {
    if (project.timeline.some(clip => clip.id === id && mediaUrl(clip.source) === player.getAttribute('src'))) continue;
    player.pause();
    player.removeAttribute('src');
    pictureLayers.delete(id);
  }
  for (const [id,player] of audioLayers) {
    if (project.timeline.some(clip => clip.id === id && clipAudioUrl(clip) === player.getAttribute('src'))) continue;
    player.pause();
    player.removeAttribute('src');
    playerSignals.get(player)?.signal.disconnect();
    playerSignals.get(player)?.gain.disconnect();
    audioLayers.delete(id);
  }
  renderTimelineTracks(fitTracks);
  const duration = clipTotal();
  byId('runtime').textContent = duration ? time(duration) + 's' : '';
  byId('empty-preview-text').textContent=project.sources.length
    ? 'Drag media to the timeline or double-click it in Media.'
    : 'Drop video, audio or images anywhere.';
  byId('viewer-import').hidden=project.sources.length>0;
  byId('play-cut').disabled = sourcePreview ? sourcePreview.image : !duration;
  updateExportAvailability();
  byId('even-levels').disabled = !project.timeline.some(clip => clip.kind === 'audio');
  byId('undo').disabled = !project.canUndo;
  byId('redo').disabled = !project.canRedo;
  byId('total-time').textContent = '/ ' + tc(duration);
  renderLibrary();
  const track = byId('soundtrack');
  track.replaceChildren(option('None', ''), ...project.sources.filter(source => source.audio).map(source => option(source.title, source.id)));
  track.value = project.soundtrack || '';
  for (const id of selection) if (!project.timeline.some(clip => clip.id === id)) selection.delete(id);
  if (!selection.has(selected)) {
    selected = selection.values().next().value || (selected === undefined ? basePictures()[0]?.id : null);
    if (selected) {
      const group = linkedSelection(new Set([selected]));
      for (const clip of group) selection.add(clip.id);
      if (!group.length) selection.add(selected);
    }
  }
  updateSelection();
}
function renderTimelineTracks(fitTracks = false) {
  timelineBody.dataset.filmstrip=String(toggled('filmstrip'));
  const old = timelineRows;
  const priorScroll = old ? timelineBody.scrollTop - old.top : 0;
  const {tracks,captions} = project.sequence;
  const video = tracks.filter(track => track.name.startsWith('V')).reverse();
  const audio = tracks.filter(track => track.name.startsWith('A'));
  const sizes = [timelineBody.clientHeight - TimelineView.ruler, video.length, audio.length, Boolean(captions)];
  const fit = fitTracks || !old;
  const view = fit ? TimelineView.fitRows(...sizes) : {layout:TimelineView.rows(old.height,...sizes)};
  const container = byId('timeline-tracks');
  if (container.contains(document.activeElement)) timelineBody.focus({preventScroll:true});
  container.replaceChildren();
  pxPerTick = Math.max(pxPerTick,minimumTimelineZoom());
  drawRuler();
  if (captions) renderCaptionLane(container);
  addTrackButton(container,'Video');
  for (const track of video) renderLane(container,track);
  container.append(item('div','timeline-divider',''));
  for (const track of audio) renderLane(container,track);
  addTrackButton(container,'Audio');
  if (retimeProjection) paintRetimeGeometry();
  const scroll = fit ? view.scroll : priorScroll + view.layout.above - old.above;
  setRowView(view.layout,scroll);
  if (fit) timelineBody.scrollLeft = 0;
  drawRulerTicks();
  drawTimelineBars();
}
function setRowView(layout, scroll) {
  timelineRows = layout;
  timelineBody.style.setProperty('--row-height',layout.height+'px');
  byId('timeline-above').style.height = layout.top+'px';
  byId('timeline-below').style.height = Math.max(0,layout.world - layout.top - layout.content)+'px';
  timelineBody.scrollTop = scroll + layout.top;
  scheduleTimelineMedia();
}

function scheduleTimelineMedia() {
  if (timelineMediaPaint != null) return;
  timelineMediaPaint = requestAnimationFrame(() => {
    timelineMediaPaint = null;
    paintFilmstrips();
    paintWaveforms();
  });
}

function loadWaveform(url) {
  if (loadingWaveforms>=2) return;
  loadingWaveforms++;
  const entry={};
  audioWaveforms.set(url,entry);
  entry.request=fetch(url).then(async response=>{
    const rate=Number(response.headers.get('x-editroom-waveform-rate'));
    if (!response.ok || !(rate>0)) return;
    const bytes=await response.arrayBuffer();
    if (bytes.byteLength%4===0) {
      entry.rate=rate;
      entry.samples=new DataView(bytes);
    }
  }).catch(()=>{}).finally(()=>{
    loadingWaveforms--;
    scheduleTimelineMedia();
  });
  return entry;
}

function paintWaveforms() {
  if (!project || byId('studio').hidden) return;
  const clips=new Map(project.timeline.map(clip=>[clip.id,clip]));
  const projected=new Map(retimeProjection?.clips.map(clip=>[clip.id,clip]));
  const visible=new Set();
  const scroll=timelineBody.scrollLeft, right=scroll+timelineViewWidth();
  const bounds=timelineBody.getBoundingClientRect();
  const top=bounds.top+byId('ruler').offsetHeight, bottom=bounds.top+timelineBody.clientHeight;
  for (const canvas of byId('timeline-tracks').querySelectorAll('.clip-waveform')) {
    const button=canvas.parentElement;
    const clip=clips.get(button.dataset.clip);
    if (!clip) continue;
    const left=button.offsetLeft+button.clientLeft;
    const first=Math.max(0,Math.floor(scroll-left)), last=Math.min(button.clientWidth,Math.ceil(right-left));
    const box=button.getBoundingClientRect(), height=button.clientHeight;
    if (last<=first || height<6 || box.bottom<=top || box.top>=bottom) {
      canvas.hidden=true;
      continue;
    }
    const url=sourceUrl('waveform-levels',clip.source);
    visible.add(url);
    if (!audioWaveforms.has(url)) loadWaveform(url);
    const entry=audioWaveforms.get(url);
    if (entry) {
      audioWaveforms.delete(url);
      audioWaveforms.set(url,entry);
    }
    const levels=entry?.samples;
    canvas.hidden=!levels?.byteLength;
    if (canvas.hidden) continue;
    const projectedClip=projected.get(clip.id);
    const timing=projectedClip && Object.hasOwn(projectedClip,'source_clock')
      ? {...clip,source_clock:projectedClip.source_clock} : clip;
    const duration=projectedClip?.duration ?? length(clip);
    const gain=10**(pictureEffects(clip).gain_db/20);
    canvas.style.left=first+'px';
    drawWaveform(canvas,entry,timing,duration,first,last,height,gain);
  }
  let bytes=0;
  for (const entry of audioWaveforms.values()) bytes+=entry.samples?.byteLength ?? 0;
  for (const [url,entry] of audioWaveforms) {
    if (audioWaveforms.size<=32 && bytes<=40*1024*1024) break;
    if (visible.has(url)) continue;
    bytes-=entry.samples?.byteLength ?? 0;
    audioWaveforms.delete(url);
  }
}

function drawWaveform(canvas,{rate,samples:levels},clip,duration,first,last,height,gain) {
  const width=last-first;
  if (canvas.width!==width) canvas.width=width;
  if (canvas.height!==height) canvas.height=height;
  const context=canvas.getContext('2d');
  context.clearRect(0,0,width,height);
  const bucketAt=x=>sourcePosition(clip,Math.min(duration,Math.max(0,(first+x)/pxPerTick))).offset*rate/TICKS_PER_SECOND;
  const samples=levels.byteLength/4;
  const averaged=[];
  let prior=bucketAt(-2);
  for (let x=-2;x<=width+2;x++) {
    const next=bucketAt(x+1);
    const from=Math.floor(Math.min(prior,next));
    const to=Math.max(from,Math.ceil(Math.max(prior,next))-1);
    prior=next;
    const step=Math.max(1,Math.floor((to-from)/32));
    let power=0, count=0;
    for (let i=from;i<=to;i+=step) {
      if (i<samples) power+=levels.getFloat32(i*4,true)**2;
      count++;
    }
    averaged.push(Math.sqrt(power/count));
  }
  const base=height-1;
  context.beginPath();
  context.moveTo(0,base);
  for (let x=0;x<=width;x++) {
    const soft=(averaged[x]+averaged[x+1]+averaged[x+2]+averaged[x+3]+averaged[x+4])/5;
    const amplitude=Math.max(.75,Math.min(1,soft*2*gain)*(base-2)*.92);
    context.lineTo(x,base-amplitude);
  }
  context.lineTo(width,base);
  context.closePath();
  context.fillStyle=toggled('filmstrip') ? 'rgba(191,204,209,.7)' : 'rgba(224,247,230,.55)';
  context.fill();
}

function gradedFilmstripFrame(frame,grade) {
  if (!grade) return frame;
  let grades = gradedFilmstripFrames.get(frame);
  if (!grades) {
    grades = new WeakMap();
    gradedFilmstripFrames.set(frame,grades);
  }
  if (grades.has(grade)) return grades.get(grade);
  const result = document.createElement('canvas');
  result.width = frame.width;
  result.height = frame.height;
  const colored = gradePreview.paint(frame,grade,frame.width,frame.height);
  result.getContext('2d',{alpha:false}).drawImage(colored,0,0);
  grades.set(grade,result);
  return result;
}

function paintFilmstrips() {
  if (!project || retimeProjection || byId('studio').hidden || !toggled('filmstrip')) return;
  const jobs = new Map();
  const viewStart = timelineBody.scrollLeft;
  const viewEnd = viewStart + timelineViewWidth();
  const {top} = timelineBody.getBoundingClientRect();
  const trackTop = top + byId('ruler').offsetHeight;
  const trackBottom = top + timelineBody.clientHeight;
  const clips = new Map(project.timeline.map(clip => [clip.id,clip]));
  const sources = new Map(project.sources.map(asset => [asset.id,asset]));
  const pps = pxPerTick * TICKS_PER_SECOND;
  for (const strip of byId('timeline-tracks').querySelectorAll('.filmstrip-window')) {
    const button = strip.parentElement;
    const canvas = strip.firstElementChild;
    const clip = clips.get(button.dataset.clip);
    const asset = clip && sources.get(clip.source);
    const left = button.offsetLeft + button.clientLeft;
    const width = button.clientWidth;
    const first = Math.max(0,Math.floor(viewStart - left));
    const last = Math.min(width,Math.ceil(viewEnd - left));
    if (!asset?.duration || width < 12 || last <= first) { canvas.hidden = true; continue; }
    const bounds = button.getBoundingClientRect();
    const height = button.clientHeight;
    if (bounds.bottom <= trackTop || bounds.top >= trackBottom || height < 1) { canvas.hidden = true; continue; }
    if (canvas.width !== last-first) canvas.width = last-first;
    if (canvas.height !== height) canvas.height = height;
    canvas.style.left = first+'px';
    canvas.hidden = false;
    const context = canvas.getContext('2d');
    context.clearRect(0,0,canvas.width,height);
    const effects = pictureEffects(clip);
    const grade = gradeForPicture(clip,effects.grade);
    const url = sourceUrl('media',asset.id,asset.version);
    const placements = StudioFilmstrip.tiles({first,last,height,pps,
      aspect:filmstripFrames.aspects.get(url) || 16/9, start:clip.start/TICKS_PER_SECOND,
      end:clip.end/TICKS_PER_SECOND, duration:length(clip)/TICKS_PER_SECOND,
      sourceDuration:asset.duration/TICKS_PER_SECOND, speed:effects.speed || 1,
      reverse:effects.reverse, freeze:effects.freeze, still:asset.image,
      ramp:clip.source_clock?.length > 1 && !effects.freeze ? seconds => sourcePosition(clip,ticks(seconds)).offset/TICKS_PER_SECOND : null,
    });
    for (const tile of placements) {
      const key = `${url}:${tile.ms}`;
      if (jobs.size >= 360 && !jobs.has(key)) continue;
      jobs.set(key,{key,url,ms:tile.ms});
      const frame = filmstripFrames.get(url,tile.ms);
      if (!frame) continue;
      const image = gradedFilmstripFrame(frame,grade);
      const sw = Math.min(image.width,image.height * tile.width / height);
      const sh = Math.min(image.height,image.width * height / tile.width);
      context.drawImage(image,(image.width-sw)/2,(image.height-sh)/2,sw,sh,tile.left-first,0,tile.width,height);
    }
  }
  filmstripFrames.request(jobs);
}
function addTrackButton(container, name) {
  const label = item('div', 'track-label timeline-track-add', '');
  const button = item('button','button','+ ' + name);
  button.type = 'button';
  button.setAttribute('aria-label','Add ' + name.toLowerCase() + ' track');
  button.onclick = () => mcp('add_track',{kind:name.toLowerCase()});
  label.append(button);
  const dropZone = item('div','track-empty','');
  dropZone.dataset.newTrack = name === 'Audio' ? 'A' : 'V';
  dropZone.oncontextmenu = event => openTimelineMenu(event,trackLayoutCommands(),'Track actions');
  container.append(label,dropZone);
}
function renderLane(container, track) {
  const audio = track.name.startsWith('A');
  const lane = item('div','clips timeline-track-row' + (audio ? ' audio-lane' : ''),'');
  const head = item('div','track-label timeline-track-row' + (audio ? ' audio-head' : ''),'');
  head.dataset.trackHeader=track.id;
  head.tabIndex=0;
  head.setAttribute('aria-label',track.name+' track');
  head.oncontextmenu=event=>openTrackMenu(event,track);
  head.append(item('strong','',track.name));
  const control = (glyph,label,key) => {
    const button = item('button','button icon',glyph);
    button.type = 'button';
    button.title = label;
    button.setAttribute('aria-label',track.name + ' ' + label.toLowerCase());
    button.setAttribute('aria-pressed',String(track[key]));
    button.onclick = () => mcp('set_track',{track:track.id,[key]:!track[key]});
    head.append(button);
  };
  if (audio) { control('M','Mute','muted'); control('S','Solo','solo'); }
  else control('◉','Hide','hidden');
  control('♙','Lock','locked');
  if (audio) {
    const duck = item('button','button icon','↓');
    duck.type = 'button';
    duck.title = track.name + ' ducking';
    duck.setAttribute('aria-label',duck.title);
    duck.setAttribute('aria-pressed',String(Boolean(track.duck_under)));
    duck.onclick = () => openDucking(track);
    head.append(trackGainPill(track), duck, meter(track.name, track.name + ' audio level'));
  }
  lane.dataset.track = track.name;
  lane.setAttribute('aria-label',track.name + (audio ? ' audio track' : ' video track'));
  for (const clip of project.timeline.filter(clip => clipTrack(clip) === track.name)) {
    const title = clipTitle(clip);
    const asset = source(clip.source);
    const button = item('button','clip ' + (clip.kind || 'video') + '-clip','');
    button.type = 'button';
    button.style.left = (timelineStart(clip.id) * pxPerTick) + 'px';
    button.style.width = Math.max(5,length(clip)*pxPerTick) + 'px';
    button.style.opacity = track.hidden || track.muted ? '.45' : '';
    if (asset?.missing_file) {
      button.dataset.missing = '';
      button.title = 'Media missing: ' + asset.missing_file;
    }
    if (picture(clip) && toggled('filmstrip') && !asset?.missing_file) {
      const strip = item('span','filmstrip-window','');
      strip.setAttribute('aria-hidden','true');
      strip.append(document.createElement('canvas'));
      button.append(strip);
    }
    if (audio && asset?.audio && !asset.missing_file) {
      const waveform=item('canvas','clip-waveform','');
      waveform.hidden=true;
      waveform.setAttribute('aria-hidden','true');
      button.append(waveform);
    }
    if (clip.kind === 'solid') button.style.setProperty('--solid',clip.effects.color);
    button.dataset.clip = clip.id;
    button.setAttribute('aria-pressed',String(selection.has(clip.id)));
    button.append(item('strong','',title),item('small','',time(length(clip))+'s'));
    renderClipControls(button,clip,track.locked);
    let dragged = false;
    button.onpointerdown = event => {
      dragged = false;
      if (track.locked || editorTool === 'cut' || event.target.closest('.trim-handle,.clip-fade-point,.clip-gain-line') ||
          event.shiftKey || event.metaKey || event.ctrlKey) return;
      beginMove(event, clip, () => {dragged = true;});
    };
    button.onclick = event => {
      if (dragged || event.target.closest('.trim-handle,.clip-fade-point,.clip-gain-line')) return;
      selectClip(clip.id,{extend:editorTool !== 'cut' && (event.shiftKey || event.metaKey || event.ctrlKey),keep:editorTool !== 'cut',seek:false});
      if (editorTool === 'cut' && !track.locked) {
        const at = timelineStart(clip.id) + Math.round((event.clientX - button.getBoundingClientRect().left) / pxPerTick);
        mcp('split_clip',{clip_id:clip.id,at:at / TICKS_PER_SECOND});
      }
    };
    button.oncontextmenu = event => showClipMenu(event,clip);
    if (!track.locked) {
      for (const side of ['start','end']) {
        const handle = item('span','trim-handle '+side,'');
        handle.title = 'Drag to trim ' + side;
        handle.onpointerdown = event => beginTrim(event, clip, side);
        button.append(handle);
      }
    }
    lane.append(button);
  }
  if (!audio) renderTransitionBands(lane,track);
  lane.onpointerdown = event => {
    if (event.button !== 0 || event.target !== lane) return;
    byId('timeline-body').focus({preventScroll:true});
    if (editorTool !== 'cut') beginMarquee(event);
  };
  lane.onpointermove = event => {
    if (!event.buttons) skimTimeline((event.clientX-lane.getBoundingClientRect().left)/pxPerTick);
  };
  lane.oncontextmenu = event => {
    if (event.target !== lane) return;
    const at = Math.max(0, Math.round((event.clientX - lane.getBoundingClientRect().left) / pxPerTick));
    const clips = project.timeline.filter(clip => clipTrack(clip) === track.name);
    const inClip = clips.some(clip => timelineStart(clip.id) <= at && at < timelineStart(clip.id) + length(clip));
    const gap = !track.locked && !inClip && clips.some(clip => timelineStart(clip.id) > at);
    const commands = track.locked ? [] : [
      ['Delete Gap',()=>mcp('delete_gap',{track:track.id,at:at/TICKS_PER_SECOND}),!gap],
      ['Close All Gaps on Track',()=>mcp('close_all_gaps',{track:track.id})]
    ];
    openTimelineMenu(event,[...commands,...trackLayoutCommands(track)],track.name+' timeline actions');
  };
  container.append(head,lane);
}
function trackLayoutCommands(track) {
  const commands = ['Video','Audio'].map(kind=>['Add '+kind+' Track',()=>mcp('add_track',{kind:kind.toLowerCase()})]);
  if (track && project.sequence.tracks.some(other=>other.name[0]===track.name[0] && other.id!==track.id)) {
    commands.push(['Delete Track',()=>mcp('delete_track',{track:track.id})]);
  }
  return commands;
}
function openTrackMenu(event,track) {
  const video=track.name.startsWith('V');
  const others=project.sequence.tracks.filter(other=>other.name[0]===track.name[0] && other.id!==track.id);
  const commands=[];
  if (!video) {
    const apply = fields => mcp('set_track',{track:track.id,...fields});
    commands.push(['Duck Under',[
      ['None',()=>apply({duck_under:null}),false,!track.duck_under],
      ...others.map(voice=>[voice.name,()=>apply({duck_under:voice.id}),false,voice.id===track.duck_under]),
      ...[6,12,18,24].map(db=>['Down '+db+' dB',()=>apply({duck_db:-db}),false,track.duck_db===-db])
    ]]);
  }
  commands.push(['Close All Gaps on Track',()=>mcp('close_all_gaps',{track:track.id}),track.locked]);
  openTimelineMenu(event,[...commands,...trackLayoutCommands(track)],track.name+' track actions');
}
function roundedTrackGain(db) {
  return TimelineView.clamp(Math.sign(db)*Math.round(Math.abs(db)*10)/10,-60,12);
}
function trackGain(track) {
  return trackGainDraft?.id===track.id ? trackGainDraft.gain : track.gain_db;
}
function trackGainPill(track) {
  const pill=item('div','track-level','');
  const button=item('button','button track-gain','');
  const field=item('input','track-gain','');
  button.type='button';
  button.title='Track level — drag, or click to type';
  field.type='text';
  field.inputMode='decimal';
  field.hidden=true;
  field.setAttribute('aria-label',track.name+' gain in dB');
  const draw=db=>{
    const label=db<=-59.5 ? '−∞' : (db>0?'+':'')+db.toFixed(1);
    button.textContent=label;
    button.setAttribute('aria-label',track.name+' gain, '+label+' dB');
  };
  draw(trackGain(track));
  const save = async (value, before=trackGain(track)) => {
    const gain=roundedTrackGain(value);
    if (gain===before) {draw(gain);return;}
    const draft={id:track.id,gain};
    trackGainDraft=draft;
    draw(gain);
    const editor=projectId;
    const apply=()=>mcp('set_track',{track:track.id,gain_db:gain},{editor});
    trackGainWrites=trackGainWrites.then(apply,apply);
    const result=await trackGainWrites;
    if (trackGainDraft===draft) trackGainDraft=null;
    if (!result) draw(track.gain_db);
  };
  let dragged=false;
  button.onpointerdown=event=>{
    if (event.button!==0) return;
    dragged=false;
    const base=trackGain(track);
    const draft={id:track.id,gain:base};
    const value=pointer=>roundedTrackGain(base+(pointer.clientX-event.clientX)*.15);
    timelineGesture(event,{
      update(pointer) {
        dragged=true;
        draft.gain=value(pointer);
        trackGainDraft=draft;
        draw(draft.gain);
      },
      complete(pointer,moved) {
        if (moved) {
          dragged=true;
          timelineBody.focus({preventScroll:true});
          save(value(pointer),base);
        }
      },
      cancel() {
        if (trackGainDraft===draft) trackGainDraft=null;
        draw(trackGain(track));
      }
    });
  };
  button.onclick=event=>{
    if (dragged && event.detail) return;
    field.value=(trackGain(track)<=-59.5 ? -60 : trackGain(track)).toFixed(1);
    button.hidden=true;
    field.hidden=false;
    field.focus({preventScroll:true});field.select();
  };
  function finish(commit,focus=false) {
    if (field.hidden) return;
    const text=field.value.replaceAll(',','.').replaceAll('−','-').replace(/[^0-9.\-]/g,'');
    field.hidden=true;
    button.hidden=false;
    if (focus) timelineBody.focus({preventScroll:true});
    if (commit && text && Number.isFinite(Number(text))) save(Number(text));
  }
  field.onblur=()=>finish(true);
  field.onkeydown=event=>{
    if (!['Enter','Escape'].includes(event.key)) return;
    event.preventDefault();event.stopPropagation();
    finish(event.key==='Enter',true);
  };
  pill.append(button,field);
  return pill;
}
const playheadTicks = () => frameAt;
const selectionArgs = (ids = selection) => ({clip_ids:[...ids]});
const timelineLanes = () => [...byId('timeline-tracks').querySelectorAll('[data-track]')];
const trackFor = clip => project.sequence.tracks.find(track => track.name === clipTrack(clip));
const timelineEnd = clip => timelineStart(clip.id) + length(clip);

function linkedSelection(ids) {
  const links = new Set(project.timeline.filter(clip => ids.has(clip.id) && !trackFor(clip)?.locked).map(clip => clip.link).filter(Boolean));
  return project.timeline.filter(clip => !trackFor(clip)?.locked && (ids.has(clip.id) || links.has(clip.link)));
}

function gainLinePosition(db) {
  return db>=0 ? .3-db/12*.2 : .3-db/60*.62;
}
function clipGainAt(position) {
  const v=Math.max(.1,Math.min(.92,position));
  const db=Math.round(10*(v<=.3 ? (.3-v)/.2*12 : -((v-.3)/.62)*60))/10;
  return db<=-59.5 ? -60 : db;
}
function renderClipControls(button,clip,locked) {
  const fx=clip.effects;
  const width=Math.max(5,length(clip)*pxPerTick);
  const showFade=fx.fade_in_frames>0 || fx.fade_out_frames>0;
  if (!showFade && clip.kind!=='audio') return;
  if (showFade) {
    const canvas=item('canvas','clip-fade-curve','');
    canvas.setAttribute('aria-hidden','true');
    button.append(canvas);
    const order=fx.fade_in_frames>fx.fade_out_frames ? ['out','in'] : ['in','out'];
    if (!locked && width>36) for (const side of order) {
      if (!fx['fade_'+side+'_frames']) continue;
      const dot=item('span','clip-fade-point','');
      dot.dataset.fade=side;
      dot.onpointerdown=event=>dragClipEffect(event,button,clip,side);
      button.append(dot);
    }
  }
  if (clip.kind==='audio' && width>8) {
    const line=item('span','clip-gain-line','');
    if (!locked) {
      line.onpointerdown=event=>dragClipEffect(event,button,clip,'gain');
      line.ondblclick=event=>{
        event.stopPropagation();
        savePicture('set_audio',{clip_id:clip.id,gain_db:0});
      };
    }
    line.classList.toggle('read-only',locked);
    button.append(line);
  }
  drawClipControls(button,clip,fx);
}
function drawClipControls(button,clip,fx) {
  const width=Math.max(5,length(clip)*pxPerTick);
  const canvas=button.querySelector('.clip-fade-curve');
  if (canvas) {
    const w=Math.min(2048,Math.ceil(width)), h=48;
    canvas.width=w;canvas.height=h;
    const context=canvas.getContext('2d');
    for (const side of ['in','out']) {
      const time=fx['fade_'+side+'_frames'];
      if (!time) continue;
      const lengthPx=time/length(clip)*w;
      const edge=side==='in' ? 0 : w;
      const curve=fx['fade_'+side+'_curve'];
      context.beginPath();
      context.moveTo(edge,h);
      for (let i=1;i<=24;++i) {
        const progress=i/24;
        context.lineTo(edge+(side==='in'?1:-1)*progress*lengthPx,h*(1-fadeCurve(curve,progress)));
      }
      context.strokeStyle='#ffedf5d9';
      context.stroke();
      context.lineTo(edge,0);
      context.closePath();
      context.fillStyle='#08050b70';
      context.fill();
    }
  }
  for (const dot of button.querySelectorAll('[data-fade]')) {
    const side=dot.dataset.fade;
    const at=fx['fade_'+side+'_frames']*pxPerTick;
    const position=side==='in' ? at : width-at;
    dot.style.left=Math.max(6,Math.min(width-6,position))+'px';
    dot.title=(side==='in'?'Fade in':'Fade out')+' · '+time(fx['fade_'+side+'_frames'])+'s';
  }
  const line=button.querySelector('.clip-gain-line');
  if (line) {
    line.style.top=100*gainLinePosition(fx.gain_db)+'%';
    line.title=(fx.gain_db===-60?'−∞':fx.gain_db.toFixed(1))+' dB';
  }
}
function dragClipEffect(event,button,clip,edge) {
  if (event.button!==0 || editorTool==='cut') return;
  event.preventDefault();
  event.stopPropagation();
  const box=button.getBoundingClientRect();
  const base=clip.effects;
  const gain=edge==='gain';
  const key=gain?'gain_db':'fade_'+edge+'_frames';
  const opposite=edge==='in'?'fade_out_frames':'fade_in_frames';
  const group=fadeGroup(clip);
  const input=gain ? byId('clip-gain') : fadeGroups[group].duration[edge];
  const reflectsClip=()=>gain ? playbackSelection().audio[0]?.id===clip.id : fadeAnchor(group)?.id===clip.id;
  let pending, value, tooltip;
  function update(pointer) {
    if (gain) {
      value=clipGainAt(gainLinePosition(base.gain_db)+(pointer.clientY-event.clientY)/box.height);
    } else {
      const x=pointer.clientX-button.getBoundingClientRect().left;
      const delta=(edge==='in'?x:box.width-x)/pxPerTick;
      const other=edge==='in' ? base[opposite] : ticks(fadeDuration(clip,'in'));
      value=Math.min(Math.max(0,length(clip)-other),Math.max(0,onFrame(delta)));
    }
    const changes={[key]:value};
    pending=setPictureDraft(clip,changes);
    drawClipControls(button,clip,{...base,...changes});
    if (reflectsClip()) {
      input.value=gain ? value : (value/TICKS_PER_SECOND).toFixed(3);
    }
    if (!tooltip) {
      tooltip=item('span','clip-effect-readout','');
      document.body.append(tooltip);
    }
    tooltip.textContent=gain ? (value===-60?'−∞':value.toFixed(1))+' dB' : time(value)+'s';
    tooltip.style.left=(pointer.clientX+10)+'px';
    tooltip.style.top=(pointer.clientY-26)+'px';
  }
  timelineGesture(event,{
    update,
    complete(pointer,moved) {
      if (!moved) return;
      update(pointer);
      tooltip?.remove();
      savePicture('set_audio',{clip_id:clip.id,[gain?'gain_db':'fade_'+edge]:gain?value:value/TICKS_PER_SECOND},pending);
    },
    cancel() {
      tooltip?.remove();
      tooltip=null;
      clearPictureDraft(pending);
      drawClipControls(button,clip,base);
      if (reflectsClip()) input.value=gain ? base.gain_db : fadeDuration(clip,edge);
    }
  });
}
function snapOffset(delta, edges, excluded = new Set(), {includePlayhead=true,ignore,pixels=8} = {}) {
  const points=[project.sequence.in_point,project.sequence.out_point].filter(point=>point!=null);
  const candidates = [0,...project.sequence.markers.map(marker=>marker.frame),...points,
    ...project.timeline.filter(clip=>!excluded.has(clip.id)).flatMap(clip=>[timelineStart(clip.id),timelineEnd(clip)])];
  if (includePlayhead) candidates.push(playheadTicks());
  let adjustment = pixels / pxPerTick;
  let target;
  for (const edge of edges) for (const candidate of candidates) {
    if (candidate===ignore) continue;
    const distance = candidate-edge-delta;
    if (Math.abs(distance)<Math.abs(adjustment) || target===undefined && Math.abs(distance)===Math.abs(adjustment)) {
      adjustment=distance;
      target=candidate;
    }
  }
  return target===undefined ? {delta} : {delta:delta+adjustment,target};
}
function snapFrame(value) { return snapOffset(value,[0]).delta; }

function timelineGesture(event, {update,complete,cancel,autoscroll=false,cancelOnEscape=false}) {
  if (presentedFrame && event.currentTarget!==byId('ruler')) return;
  const element = event.currentTarget;
  const id = event.pointerId;
  const editor = projectId;
  const revision = project.revision;
  let moved = false;
  element.setPointerCapture(id);
  function dragging(pointer) {
    if (pointer.pointerId !== id) return false;
    moved ||= Math.hypot(pointer.clientX-event.clientX,pointer.clientY-event.clientY) > 3;
    return moved;
  }
  let pointerAt,scrollFrame;
  const viewport=byId('timeline-body');
  function autoScroll() {
    scrollFrame=null;
    const rect=viewport.getBoundingClientRect();
    const header=viewport.querySelector('.ruler-label').offsetWidth;
    const drift=(value,from,to)=>{
      const zone=30;
      const amount=value<from+zone ? value-from-zone : value>to-zone ? value-to+zone : 0;
      return Math.max(-23,Math.min(23,Math.round(amount*.38)));
    };
    const x=viewport.scrollLeft,y=viewport.scrollTop;
    viewport.scrollLeft+=drift(pointerAt.clientX,rect.left+header,rect.right);
    if (autoscroll!=='x') viewport.scrollTop+=drift(pointerAt.clientY,rect.top+TimelineView.ruler,rect.bottom);
    if (x!==viewport.scrollLeft || y!==viewport.scrollTop) {
      update(pointerAt);
      scrollFrame=requestAnimationFrame(autoScroll);
    }
  }
  function move(pointer) {
    if (!dragging(pointer)) return;
    update(pointer);
    if (autoscroll) {
      pointerAt=pointer;
      if (scrollFrame==null) scrollFrame=requestAnimationFrame(autoScroll);
    }
  }
  function finish(pointer) {
    if (pointer.pointerId !== id) return;
    const current=projectId === editor && project.revision === revision;
    const changed=dragging(pointer);
    cleanup(current && changed);
    if (current) complete(pointer,changed);
  }
  function escape(key) {
    if (key.key!=='Escape') return;
    key.preventDefault();
    key.stopPropagation();
    cleanup(false);
    if (element.hasPointerCapture(id)) element.releasePointerCapture(id);
  }
  function cleanup(committing) {
    if (scrollFrame!=null) cancelAnimationFrame(scrollFrame);
    element.removeEventListener('pointermove',move);
    element.removeEventListener('pointerup',finish);
    element.removeEventListener('pointercancel',cleanup);
    element.removeEventListener('lostpointercapture',cleanup);
    if (cancelOnEscape) document.removeEventListener('keydown',escape,true);
    cancel?.(committing===true);
  }
  element.addEventListener('pointermove',move);
  element.addEventListener('pointerup',finish);
  element.addEventListener('pointercancel',cleanup);
  element.addEventListener('lostpointercapture',cleanup);
  if (cancelOnEscape) document.addEventListener('keydown',escape,true);
}

function timelineOverlay() {
  const node = item('div','timeline-drag-overlay','');
  node.setAttribute('aria-hidden','true');
  document.body.append(node);
  return node;
}
function boxStyle(node,left,top,width,height) {
  Object.assign(node.style,{left:left+'px',top:top+'px',width:width+'px',height:height+'px'});
  return node;
}
function gestureClip(title,lane,at,duration) {
  if (!lane) return null;
  const row = lane.getBoundingClientRect();
  const view = byId('timeline-body').getBoundingClientRect();
  const contentLeft = view.left + byId('timeline-body').querySelector('.track-label').offsetWidth;
  if (row.bottom < view.top+TimelineView.ruler || row.top > view.bottom) return null;
  const left = Math.max(contentLeft,row.left + at*pxPerTick);
  const right = Math.min(view.right,row.left + (at+duration)*pxPerTick);
  if (right <= left) return null;
  const top = Math.max(view.top+TimelineView.ruler,row.top+3);
  return boxStyle(item('span','timeline-ghost',title),left,top,right-left,Math.max(0,Math.min(view.bottom,row.bottom-3)-top));
}
function showTrimWaveform(ghost,shape,clip,row) {
  if (!shape.audio_window || source(clip.source)?.missing_file) return;
  const url=sourceUrl('waveform-levels',clip.source);
  const levels=audioWaveforms.get(url) || loadWaveform(url);
  if (!levels) return;
  levels.request.then(()=>{
    if (!ghost.isConnected || !levels.samples?.byteLength || ghost.clientHeight<6) return;
    const start=row.getBoundingClientRect().left+shape.start*pxPerTick;
    const first=Math.max(0,Math.round(ghost.getBoundingClientRect().left-start));
    const wave=item('canvas','clip-waveform','');
    const title=item('span','timeline-ghost-title',ghost.textContent);
    ghost.setAttribute('data-waveform','');
    ghost.replaceChildren(wave,title);
    drawWaveform(wave,levels,{...clip,...shape.audio_window},shape.duration,
      first,first+ghost.clientWidth,ghost.clientHeight,10**(clip.effects.gain_db/20));
  });
}
function gestureSnap(lane,target) {
  if (target === undefined) return null;
  const view = byId('timeline-body').getBoundingClientRect();
  const at = lane.getBoundingClientRect().left + target * pxPerTick;
  return boxStyle(item('span','timeline-snap',''),at,view.top,1,view.height);
}

function mediaDropTarget(event) {
  if (!project || presentedFrame || pendingMediaDrop) return;
  const types = event.dataTransfer.types;
  const external = types.includes('Files');
  const graphic = !external && graphicDrags.find(({type})=>types.includes(type));
  if (!external && !graphic && !types.includes(sourceDragType)) return;
  if (graphic && directorOwnsCut()) return;
  const viewport = timelineBody.getBoundingClientRect();
  const ruler = byId('ruler');
  const header = timelineBody.querySelector('.ruler-label').offsetWidth;
  if (event.clientX < viewport.left + header || event.clientY < viewport.top + TimelineView.ruler) return;
  const lane = event.target.closest('[data-track],[data-new-track]');
  const track = project.sequence.tracks.find(track=>track.name===lane?.dataset.track);
  const newKind = lane?.dataset.newTrack;
  const ranked = kind=>project.sequence.tracks.filter(track=>track.name.startsWith(kind));
  const kind = newKind || track?.name[0];
  const rank = newKind ? ranked(newKind).length : track ? Math.max(0,ranked(kind).indexOf(track)) : 0;
  const chooseLane = kind=>{
    const tracks = ranked(kind);
    const nominal = tracks[Math.min(rank,tracks.length-1)];
    const available = nominal?.locked ? tracks.find(track=>!track.locked) : nominal;
    return newKind===kind || !available
      ? timelineBody.querySelector('[data-new-track="'+kind+'"]')
      : timelineLanes().find(lane=>lane.dataset.track===available.name);
  };
  const assets = external || graphic ? [] : (draggedSources || []).map(source).filter(Boolean);
  const duration = asset=>asset.image ? asset.duration :
    Math.min(asset.duration,Math.max(frameStep(),Math.floor(asset.duration/frameStep())*frameStep()));
  const positions = assets.map(asset=>({asset,duration:duration(asset)}));
  const span = positions.reduce((sum,entry)=>sum+entry.duration,0) || ticks(5);
  const raw = Math.max(0,Math.round((event.clientX-ruler.getBoundingClientRect().left)/pxPerTick));
  const snap = toggled('snap') ? snapOffset(raw,[0,span]) : null;
  const anchored = snap?.target != null && snap.delta >= 0;
  return {project:projectId, external, graphic, positions, at:anchored ? snap.delta : onFrame(raw),
    snap:anchored ? snap.target : undefined, video:chooseLane('V'),audio:graphic ? null : chooseLane('A'),
    overAudio:kind==='A', track:track?.id || (newKind === 'V' ? 'new_video' : newKind === 'A' ? 'new_audio' : undefined)};
}

function clearMediaDropOverlay() {
  mediaDropOverlay?.remove();
  mediaDropOverlay = null;
}

timelineBody.addEventListener('dragover', event=>{
  const target = mediaDropTarget(event);
  if (!target) {clearMediaDropOverlay();return;}
  event.preventDefault();
  event.dataTransfer.dropEffect = 'copy';
  mediaDropOverlay ||= timelineOverlay();
  const ghosts = [];
  let at = target.at;
  if (target.graphic) {
    ghosts.push(gestureClip(target.graphic.label,target.video,at,ticks(5)));
  } else if (target.external) {
    ghosts.push(gestureClip('Import media',target.overAudio ? target.audio : target.video,at,ticks(5)));
  } else {
    for (const {asset,duration} of target.positions) {
      if (asset.video) ghosts.push(gestureClip(asset.title,target.video,at,duration));
      if (asset.audio) ghosts.push(gestureClip(asset.title,target.audio,at,duration));
      at += duration;
    }
  }
  ghosts.push(gestureSnap(byId('ruler'),target.snap));
  mediaDropOverlay.replaceChildren(...ghosts.filter(Boolean));
});
timelineBody.addEventListener('dragleave',event=>{
  if (!timelineBody.contains(event.relatedTarget)) clearMediaDropOverlay();
});
timelineBody.addEventListener('drop',async event=>{
  const target = mediaDropTarget(event);
  clearMediaDropOverlay();
  if (!target) return;
  event.preventDefault();
  const {project:editor,at,track} = target;
  if (target.graphic) {
    const graphic = target.graphic;
    const videoTrack = target.video?.dataset.newTrack ? 'new_video' : target.video?.dataset.track;
    if (!videoTrack || event.dataTransfer.getData(graphic.type) !== graphic.label) return;
    const result = await addGraphic(graphic.tool,{...graphic.fields,start:at/TICKS_PER_SECOND,track:videoTrack});
    if (result && projectId===editor) timelineBody.focus({preventScroll:true});
    return;
  }
  const drop = {editor,at,track};
  pendingMediaDrop = drop;
  try {
    if (target.external) {
      drop.assets = await importMediaDrop(event.dataTransfer,editor);
    } else {
      const data = event.dataTransfer.getData(sourceDragType);
      drop.assets = data.split(',').filter(Boolean);
    }
    finishMediaDrop();
  } catch(error) {
    if (pendingMediaDrop===drop) pendingMediaDrop=null;
    if (projectId===editor) status(error.message);
  }
});

async function finishMediaDrop() {
  const drop = pendingMediaDrop;
  if (!drop?.assets || savedProject?.id !== drop.editor || !drop.assets.length) {
    if (drop?.assets) pendingMediaDrop = null;
    return;
  }
  if (savedProject.sequence_offer || state.runs.some(run=>run.project===drop.editor && run.status==='running')) return;
  pendingMediaDrop = null;
  const result = await mcp('place_media',{asset_ids:drop.assets,start:drop.at/TICKS_PER_SECOND,track:drop.track},{editor:drop.editor});
  if (result && projectId===drop.editor) {
    setSelection(result.added.map(clip=>clip.id));
    timelineBody.focus({preventScroll:true});
  }
}

const libraryDropArea = byId('media-title').closest('.library');
function acceptsLibraryDrop(event) {
  return event.dataTransfer.types.includes('Files') && !event.target.closest('input[type="file"]');
}
function canImportDroppedMedia() {
  return project && !byId('studio').hidden && !presentedFrame && !directorOwnsCut() && !document.querySelector('dialog[open]');
}
function importIntoLibrary(transfer) {
  const editor = projectId;
  importMediaDrop(transfer,editor,true).catch(error=>{
    if (projectId === editor) status(error.message);
  });
}
libraryDropArea.addEventListener('dragover',event=>{
  if (!canImportDroppedMedia() || !acceptsLibraryDrop(event)) return;
  event.preventDefault();
  event.dataTransfer.dropEffect='copy';
  libraryDropArea.classList.add('media-drop-target');
});
libraryDropArea.addEventListener('dragleave',event=>{
  if (!libraryDropArea.contains(event.relatedTarget)) libraryDropArea.classList.remove('media-drop-target');
});
libraryDropArea.addEventListener('drop',event=>{
  libraryDropArea.classList.remove('media-drop-target');
  if (!canImportDroppedMedia() || !acceptsLibraryDrop(event)) return;
  event.preventDefault();
  importIntoLibrary(event.dataTransfer);
});
document.addEventListener('dragover',event=>{
  if (!acceptsLibraryDrop(event)) return;
  const anywhere=!event.defaultPrevented && canImportDroppedMedia();
  if (!event.defaultPrevented) event.dataTransfer.dropEffect=anywhere ? 'copy' : 'none';
  event.preventDefault();
  libraryDropArea.classList.toggle('media-drop-target',Boolean(anywhere) || libraryDropArea.contains(event.target));
});
document.addEventListener('dragleave',event=>{
  if (!event.relatedTarget) libraryDropArea.classList.remove('media-drop-target');
});
document.addEventListener('drop',event=>{
  if (acceptsLibraryDrop(event)) {
    const anywhere=!event.defaultPrevented && canImportDroppedMedia();
    event.preventDefault();
    if (anywhere) importIntoLibrary(event.dataTransfer);
  }
  libraryDropArea.classList.remove('media-drop-target');
  clearMediaDropOverlay();
});
document.addEventListener('dragend',()=>{
  draggedSources = null;
  clearMediaDropOverlay();
});

function beginMove(event,anchor,onDrag) {
  if (event.button !== 0) return;
  const origin = event.currentTarget.closest('[data-track]');
  const grabbedAt = event.clientX - origin.getBoundingClientRect().left;
  const clips = linkedSelection(selection.has(anchor.id) ? selection : new Set([anchor.id]));
  const ids = new Set(clips.map(clip => clip.id));
  const lanes = timelineLanes();
  const anchorKind = clipTrack(anchor)[0];
  const trackGroup = kind => project.sequence.tracks.filter(track => track.name[0] === kind);
  const index = (kind,name) => trackGroup(kind).findIndex(track => track.name === name);
  const group = trackGroup(anchorKind);
  const sameKind = clips.filter(clip => clipTrack(clip)[0] === anchorKind);
  const indices = sameKind.map(clip => index(anchorKind,clipTrack(clip)));
  const partnerLinks = new Set(sameKind.map(clip => clip.link).filter(Boolean));
  const newLane = kind => byId('timeline-tracks').querySelector('[data-new-track='+kind+']');
  const earliest = Math.min(...clips.map(clip => timelineStart(clip.id)));
  const latest = Math.max(...clips.map(timelineEnd));
  let overlay;
  let currentShift = 0;
  function placement(pointer) {
    const lane = [...lanes,newLane(anchorKind)].find(row => {
      const box = row.getBoundingClientRect();
      return (row.dataset.track || row.dataset.newTrack)[0] === anchorKind && pointer.clientY >= box.top && pointer.clientY < box.bottom;
    });
    if (lane) {
      const target = lane.dataset.newTrack ? group.length : index(anchorKind,lane.dataset.track);
      const shift = Math.max(-Math.min(...indices),Math.min(group.length-Math.max(...indices),target-index(anchorKind,clipTrack(anchor))));
      if (indices.every(i => !group[i+shift]?.locked)) currentShift = shift;
    }
    const shift = currentShift;
    const raw = Math.round((pointer.clientX-origin.getBoundingClientRect().left-grabbedAt) / pxPerTick);
    const snap = toggled('snap') ? snapOffset(raw,clips.flatMap(clip => [timelineStart(clip.id),timelineEnd(clip)]),pointer.altKey ? new Set() : ids) : {delta:raw};
    const delta = Math.max(-earliest,Math.min(ticks(7200)-latest,onFrame(timelineStart(anchor.id) + snap.delta) - timelineStart(anchor.id)));
    const claimed = [];
    const destinations = clips.map(clip => {
      const kind = clipTrack(clip)[0];
      const next = trackGroup(kind)[Math.max(0,index(kind,clipTrack(clip))+shift)];
      const at = timelineStart(clip.id)+delta;
      const end = timelineEnd(clip)+delta;
      const target = next?.name || 'new-' + kind;
      let to = clipTrack(clip);
      const occupied = project.timeline.some(other => (pointer.altKey || !ids.has(other.id)) &&
        clipTrack(other) === target && timelineStart(other.id)<end && timelineEnd(other)>at)
        || claimed.some(placement => placement.to === target && placement.at<end && placement.end>at)
        || clips.some(other => other.id !== clip.id && clipTrack(other) === target &&
          timelineStart(other.id)<timelineEnd(clip) && timelineEnd(other)>timelineStart(clip.id));
      if (shift && !next?.locked && (kind === anchorKind || (partnerLinks.has(clip.link) && !occupied))) to = target;
      const placement = {clip,to,at,end};
      claimed.push(placement);
      return placement;
    });
    return {destinations,anchor:destinations.find(entry => entry.clip.id === anchor.id),
      snap:Math.abs(delta-snap.delta) <= frameStep()/2 ? snap.target : undefined};
  }
  timelineGesture(event,{
    autoscroll:true,
    update(pointer) {
      onDrag();
      if (!overlay) {
        setSelection(ids,anchor.id);
        overlay = timelineOverlay();
      }
      const {destinations,snap} = placement(pointer);
      overlay.replaceChildren(...[...destinations.map(({clip,to,at}) =>
        gestureClip(clipTitle(clip),to.startsWith('new-') ? newLane(to.at(-1)) : lanes.find(lane => lane.dataset.track === to),at,length(clip))),gestureSnap(origin,snap)].filter(Boolean));
    },
    async complete(pointer,moved) {
      if (!moved) return;
      onDrag();
      setSelection(ids,anchor.id);
      const target = placement(pointer).anchor;
      if (!pointer.altKey && target.at === timelineStart(anchor.id) && target.to === clipTrack(anchor)) return;
      const result = await mcp('move_clips',{...selectionArgs(ids),clip_id:anchor.id,start:target.at / TICKS_PER_SECOND,
        track:target.to.startsWith('new-') ? 'new' : target.to,duplicate:pointer.altKey});
      if (pointer.altKey && result) setSelection(result.clips.map(clip => clip.id));
    },
    cancel() {overlay?.remove();}
  });
}
function beginMarquee(event,captionOnly = false) {
  const base = event.shiftKey ? [...selection] : [];
  const baseCaptions = event.shiftKey ? [...captionSelection] : [];
  const viewport = byId('timeline-body');
  const originX = event.clientX + viewport.scrollLeft;
  const originY = event.clientY + viewport.scrollTop;
  let overlay;
  timelineGesture(event,{
    autoscroll:true,
    update(pointer) {
      const view = viewport.getBoundingClientRect();
      const x = originX - viewport.scrollLeft;
      const y = originY - viewport.scrollTop;
      const left = Math.max(Math.min(x,pointer.clientX),view.left+viewport.querySelector('.track-label').offsetWidth);
      const top = Math.max(view.top+TimelineView.ruler,Math.min(y,pointer.clientY));
      const right = Math.min(view.right,Math.max(x,pointer.clientX));
      const bottom = Math.min(view.bottom,Math.max(y,pointer.clientY));
      overlay ||= timelineOverlay();
      overlay.replaceChildren(boxStyle(item('span','timeline-marquee',''),left,top,Math.max(0,right-left),Math.max(0,bottom-top)));
      const hits = selector => [...viewport.querySelectorAll(selector)].filter(button => {
        const box = button.getBoundingClientRect();
        return box.left < right && box.right > left && box.top < bottom && box.bottom > top;
      });
      const clips = captionOnly ? [] : linkedSelection(new Set(hits('[data-clip]').map(button => button.dataset.clip)));
      const lines = hits('[data-caption]').map(button => button.dataset.caption);
      if (captionOnly || (!clips.length && lines.length)) setCaptionSelection([...baseCaptions,...lines]);
      else setSelection([...base,...clips.map(clip => clip.id)]);
    },
    complete(_,moved) {
      if (moved || event.shiftKey) return;
      if (captionOnly) setCaptionSelection([]);
      else setSelection([]);
    },
    cancel() {overlay?.remove();}
  });
}
let trimFrameTarget;
const trimFrame = byId('trim-frame');
function showTrimFrame(frame,left) {
  if (!frame) return;
  const url = mediaUrl(frame.id);
  if (trimFrame.getAttribute('src') !== url) {
    trimFrame.hidden = true;
    trimFrame.src = url;
  }
  trimFrameTarget = frame.at/TICKS_PER_SECOND;
  const edge=left===undefined ? '' : left ? 'In ' : 'Out ';
  byId('trim-readout').textContent = edge+tc(frame.at);
  byId('trim-readout').hidden = false;
  seekTrimFrame();
}
function seekTrimFrame() {
  if (trimFrameTarget === undefined || trimFrame.readyState < 1) return;
  const at = Math.min(trimFrameTarget,Math.max(0,trimFrame.duration-1/1000));
  if (Math.abs(trimFrame.currentTime - at)>1/2000) trimFrame.currentTime = at;
  else if (!trimFrame.seeking && trimFrame.readyState >= 2) trimFrame.hidden = false;
}
trimFrame.addEventListener('loadeddata',seekTrimFrame);
trimFrame.addEventListener('loadedmetadata',seekTrimFrame);
trimFrame.addEventListener('seeked',seekTrimFrame);

function beginTrim(event,anchor,side) {
  if (event.button !== 0) return;
  event.preventDefault();
  event.stopPropagation();
  byId('timeline-body').focus({preventScroll:true});
  const clips = linkedSelection(selection.has(anchor.id) ? selection : new Set([anchor.id]));
  const ids = new Set(clips.map(clip => clip.id));
  setSelection(ids,anchor.id);
  const lanes = timelineLanes();
  const lane = event.currentTarget.closest('[data-track]');
  const grabbed = event.clientX-lane.getBoundingClientRect().left;
  const left = side === 'start';
  const edge = left ? timelineStart(anchor.id) : timelineEnd(anchor);
  const sticky = toggled('sticky');
  const ripple = sticky && (!left || clips.some(clip => timelineStart(clip.id) <= frameStep()/2 ||
    project.timeline.some(other => !ids.has(other.id) && clipTrack(other) === clipTrack(clip) &&
      Math.abs(timelineEnd(other)-timelineStart(clip.id)) <= frameStep()/2)));
  const revision = project.revision;
  const editor = projectId;
  let overlay, request, queued, timer, last;
  let active = true;
  function offset(pointer) {
    const raw = Math.round((pointer.clientX-lane.getBoundingClientRect().left-grabbed)/pxPerTick);
    const snap = toggled('snap') ? snapOffset(raw,[edge],ids) : {delta:raw};
    const delta = onFrame(edge+snap.delta)-edge;
    const limit = Math.max(0,Math.min(...clips.map(length))-frameStep());
    const earliest = Math.min(...clips.map(clip => timelineStart(clip.id)));
    const clamped = left ? Math.max(ripple ? -ticks(7200) : -earliest,Math.min(limit,delta)) : Math.max(-limit,delta);
    return {delta:clamped,target:delta === clamped ? snap.target : undefined};
  }
  function args(delta) {
    return {...selectionArgs(ids),clip_id:anchor.id,side,at:(edge+delta)/TICKS_PER_SECOND,sticky,revision};
  }
  function draw(shapes, target) {
    overlay.replaceChildren(...[...shapes.map(entry => {
      const clip = project.timeline.find(clip => clip.id === entry.id);
      const row = lanes.find(row => row.dataset.track === entry.track);
      const ghost=clip && row ? gestureClip(clipTitle(clip),row,entry.start,entry.duration) : null;
      if (ghost) showTrimWaveform(ghost,entry,clip,row);
      return ghost;
    }),gestureSnap(lane,target)].filter(Boolean));
  }
  function queuePreview(value) {
    queued = value;
    if (!timer && !request) timer = setTimeout(loadPreview,26);
  }
  async function loadPreview() {
    timer = null;
    if (!active || !queued || request || queued === last) return;
    const value = last = queued;
    const controller = request = new AbortController();
    try {
      const preview = await api(projectUrl('/trim-preview'),{
        method:'POST',data:args(value.delta),signal:controller.signal
      });
      if (active && editor === projectId && project.revision === revision && queued === value) {
        draw(preview.clips,value.target);
        showTrimFrame(preview.frame,left);
      }
    } catch (_) { /* The local trim outline is available while the server preview reconnects. */ }
    finally {
      request = null;
      if (active && queued !== last) queuePreview(queued);
    }
  }
  timelineGesture(event,{
    autoscroll:"x",
    cancelOnEscape:true,
    update(pointer) {
      overlay ||= timelineOverlay();
      const value = offset(pointer);
      if (queued && value.delta === queued.delta && value.target === queued.target) return;
      draw(clips.map(clip => ({id:clip.id,track:clipTrack(clip),
        start:timelineStart(clip.id)+(left && !ripple ? value.delta : 0),
        duration:length(clip)+(left ? -value.delta : value.delta)})),value.target);
      queuePreview(value);
    },
    complete(pointer,moved) {
      if (!moved) return;
      const {delta} = offset(pointer);
      if (delta) mcp('trim_clips',args(delta));
    },
    cancel() {
      active = false;
      request?.abort();
      clearTimeout(timer);
      overlay?.remove();
      trimFrameTarget = undefined;
      trimFrame.hidden = true;
      byId('trim-readout').hidden = true;
    }
  });
}
function loadCaptions() {
  const query=previewQuery();
  const key = [projectId,project.revision,project.library_revision,query].join('/');
  captionLayoutKey = key;
  if (!project.sequence.captions) {
    const dirty = captions.length || captionSelection.size || captionFocus;
    captions = [];
    captionSelection.clear();
    captionFocus = false;
    if (dirty) updateSelection();
    return;
  }
  api(projectUrl('/captions'+query)).then(blocks => {
    if (captionLayoutKey !== key) return;
    captions = blocks;
    const ids = new Set(blocks.map(block => block.id));
    for (const id of captionSelection) if (!ids.has(id)) captionSelection.delete(id);
    paintCaptionLane();
    updateSelection();
  }).catch(error => { if (captionLayoutKey === key) status(error.message); });
}

function writeCaptions(args) {
  const editor = projectId;
  const write = () => mcp('set_captions',args,{editor});
  captionWrites = captionWrites.then(write,write);
  return captionWrites;
}

function captionedTargets(ids) {
  if (!ids.size) return [];
  const group = linkedSelection(ids);
  return group.filter(clip => !clip.effects.freeze && source(clip.source)?.audio &&
    (clip.kind === 'audio' || (clip.kind === 'video' &&
      !project.timeline.some(sound => sound.kind === 'audio' && clip.link && sound.link === clip.link))));
}
function canAddDefaultCaptions() {
  const voice = new Set(project.sequence.tracks.map(track => track.duck_under).filter(Boolean));
  return project.timeline.some(clip => !trackFor(clip)?.locked && !clip.effects.freeze &&
    source(clip.source)?.audio && (source(clip.source)?.video || voice.has(trackFor(clip)?.id)));
}
function updateCaptionButtons() {
  const actions = captionedTargets(selection);
  const toggle = byId('caption-toggle');
  toggle.textContent = actions.some(clip => clip.captioned) ? 'Remove captions' : 'Add captions';
  toggle.disabled = !actions.length && (selection.size > 0 || !canAddDefaultCaptions());
  for (const entry of document.querySelectorAll('[data-caption]')) {
    entry.setAttribute('aria-pressed',String(captionSelection.has(entry.dataset.caption)));
  }
}
function changeClipCaptions() {
  const targets = captionedTargets(selection);
  const remove = targets.some(clip => clip.captioned);
  const args = selection.size && (remove || selection.size < project.timeline.length)
    ? {...selectionArgs(),remove} : {default_targets:true};
  return writeCaptions(args);
}
byId('caption-toggle').onclick = changeClipCaptions;

function setCaptionSelection(ids,focus = false) {
  leaveSourcePreview();
  clearLibrarySelection();
  selection.clear();
  selected = null;
  captionSelection.clear();
  ids.forEach(id => captionSelection.add(id));
  captionFocus = focus;
  updateSelection();
}
function pickCaption(block,extend) {
  const next = new Set(extend ? captionSelection : []);
  if (extend && next.has(block.id)) next.delete(block.id);
  else next.add(block.id);
  setCaptionSelection(next);
}
function openCaptionEditor(block) {
  setCaptionSelection([block.id]);
  seekTimeline((block.start+block.end)/2);
  const field = byId('caption-text');
  field.focus();
  field.select();
}
function captionTrackCommands(track) {
  return [
    [track.hidden ? 'Show Captions' : 'Hide Captions',()=>writeCaptions({hidden:!track.hidden})],
    ['Delete Caption Track',()=>writeCaptions({remove:true})],
  ];
}
function renderCaptionLane(container) {
  const track = project.sequence.captions;
  const header = item('div','track-label caption-head','');
  const name = item('button','button','C1');
  name.type = 'button';
  name.setAttribute('aria-label','Caption track');
  name.onclick = () => setCaptionSelection([],true);
  const eye = item('button','button icon','◉');
  eye.type = 'button';
  eye.setAttribute('aria-label',track.hidden ? 'Show captions' : 'Hide captions');
  eye.setAttribute('aria-pressed',String(!track.hidden));
  eye.onclick = () => writeCaptions({hidden:!track.hidden});
  header.oncontextmenu = event => openTimelineMenu(event,captionTrackCommands(track),'Caption track actions');
  header.append(name,eye);
  const lane = item('div','clips caption-lane','');
  lane.id = 'caption-lane';
  lane.dataset.hidden = String(track.hidden);
  lane.setAttribute('aria-label','Caption lane');
  lane.onpointerdown = event => {
    if (event.button !== 0 || event.target !== lane) return;
    byId('timeline-body').focus({preventScroll:true});
    beginMarquee(event,true);
  };
  lane.oncontextmenu = event => {
    if (!event.target.closest('[data-caption]')) {
      openTimelineMenu(event,captionTrackCommands(track),'Caption track actions');
    }
  };
  container.append(header,lane);
  paintCaptionLane();
}
function paintCaptionLane() {
  const lane = byId('caption-lane');
  if (!lane) return;
  if (lane.contains(document.activeElement)) byId('timeline-body').focus({preventScroll:true});
  lane.replaceChildren(...captions.map(block => {
    const button = item('button','clip caption-clip',block.text);
    button.type = 'button';
    button.title = block.text;
    button.dataset.caption = block.id;
    button.setAttribute('aria-pressed',String(captionSelection.has(block.id)));
    button.style.left = (block.start * pxPerTick) + 'px';
    button.style.width = Math.max(5,(block.end-block.start)*pxPerTick) + 'px';
    button.onclick = event => pickCaption(block,event.shiftKey || event.metaKey || event.ctrlKey);
    button.ondblclick = () => openCaptionEditor(block);
    button.oncontextmenu = event => {
      if (!captionSelection.has(block.id)) setCaptionSelection([block.id]);
      openTimelineMenu(event,[
        ['Edit Caption',()=>openCaptionEditor(block)],
        [captionSelection.size > 1 ? 'Delete Captions' : 'Delete Caption',deleteCaptionSelection],
        ['Remove Captions from Clip',()=>writeCaptions({clip_ids:[block.clip],remove:true})],
        ...captionTrackCommands(project.sequence.captions),
      ],'Caption actions');
    };
    return button;
  }));
  if (!captions.length) lane.append(item('span','caption-empty','No speech heard in captioned clips'));
}
async function deleteCaptionSelection() {
  if (!captionSelection.size) return;
  const edit = [...captionSelection].map(id => ({id,text:''}));
  const editor = projectId;
  if (await writeCaptions({edit}) && projectId === editor &&
      [...captionSelection].every(id => edit.some(change => change.id === id))) setCaptionSelection([]);
}

const captionFields = {
  size:byId('caption-size'),position:byId('caption-position'),text_color:byId('caption-text-color'),
  highlight:byId('caption-highlight'),highlight_color:byId('caption-highlight-color'),
  outline:byId('caption-outline'),outline_color:byId('caption-outline-color')
};
function renderCaptionInspector() {
  const track = project.sequence.captions;
  const show = track && (captionFocus || captionSelection.size);
  byId('caption-inspector').hidden = !show;
  if (!show) return;
  const picked = captionSelection.size===1 && captions.find(block => captionSelection.has(block.id));
  byId('caption-edit-form').hidden = !picked;
  const field = byId('caption-text');
  if (document.activeElement !== field) field.value = picked?.text || '';
  for (const [key,control] of Object.entries(captionFields)) {
    if (document.activeElement === control) continue;
    const value = captionStyle[key] ?? track[key];
    if (control.type === 'checkbox') control.checked = value;
    else if (key === 'position') control.value = value*100;
    else if (control.type === 'color') control.value = value.slice(0,7);
    else control.value = value;
  }
}
for (const [key,control] of Object.entries(captionFields)) {
  const read = () => control.type==='checkbox' ? control.checked :
    control.type==='range' ? Number(control.value)/(key==='position'?100:1) : control.value;
  control.oninput = () => {captionStyle[key] = read();drawComposed();};
  control.onchange = async () => {
    const editor = projectId;
    const value = captionStyle[key] = read();
    await writeCaptions({[key]:value});
    if (projectId === editor && captionStyle[key] === value) {
      delete captionStyle[key];
      renderCaptionInspector();
      drawComposed();
    }
  };
}
const captionTextField = byId('caption-text');
captionTextField.onfocus = () => {
  const block = captions.find(b => captionSelection.has(b.id));
  if (!block) return;
  captionEdit = {block,editor:projectId,value:block.text};
  if (frameAt < block.start || frameAt >= block.end) seekTimeline((block.start+block.end)/2);
};
captionTextField.oninput = () => {
  if (!captionEdit) return;
  captionEdit.value = captionTextField.value.trim().replace(/\s+/g,' ');
  drawComposed();
};
captionTextField.onblur = () => {
  const edit = captionEdit;
  captionEdit = null;
  drawComposed();
  if (edit && projectId === edit.editor && edit.value !== edit.block.text) {
    writeCaptions({edit:[{id:edit.block.id,text:edit.value}]});
  }
};
captionTextField.onkeydown = event => {
  if (event.key === 'Escape') {
    event.preventDefault();
    if (captionEdit) captionTextField.value = captionEdit.block.text;
    captionEdit = null;
    captionTextField.blur();
    byId('timeline-body').focus({preventScroll:true});
  } else if (event.key === 'Enter' && !event.shiftKey) {
    event.preventDefault();
    captionTextField.blur();
    byId('timeline-body').focus({preventScroll:true});
  }
};

async function mcp(name, args = {}, {quiet = false,editor = projectId} = {}) {
  if (editor===projectId && (presentedFrame || directorOwnsCut(editor))) {
    status(presentedFrame?.external ? 'The attached agent is finishing this edit.' : 'Stop Director to edit the saved timeline.');
    return null;
  }
  try {
    const data = await post('/api/projects/' + editor + '/mcp',{jsonrpc:'2.0',id:crypto.randomUUID(),method:'tools/call',params:{name,arguments:args}});
    if (data.error) throw new Error(data.error.message);
    const text = data.result.content.find(item => item.type === 'text')?.text;
    if (data.result.isError) throw new Error(text);
    if (!quiet && projectId === editor) {await refresh();status(state.autosave ? 'Saved' : 'Project updated');}
    return JSON.parse(text);
  } catch (error) {
    if (projectId === editor) status(error.message);
    return null;
  }
}
const libraryQuery = () => byId('media-search').value.trim();
const folderScope = () => libraryFolder ? libraryFolder.id + '/' + libraryFolder.path : '';
const pathContains = (root,path) => !root || path === root || path.startsWith(root + '/');
const sourceInside = (source, folder = libraryFolder) => folder && source.folder_id === folder.id && pathContains(folder.path,source.relative_path);
const parentPath = path => path.slice(0,Math.max(0,path.lastIndexOf('/')));
const baseName = path => path.split('/').at(-1);
function openLibraryFolder(id,path = '') {
  libraryFolder = id ? {id,path} : null;
  clearLibrarySelection();
  byId('media-search').value = '';
  librarySearch = null;
  queueLibrarySearch(0);
  renderLibrary();
  updateSelection();
}
function currentRoot() {
  return libraryFolder && project.folders.find(folder=>folder.id===libraryFolder.id);
}
function renderLibraryNavigation(root) {
  byId('folder-navigation').hidden = !root;
  byId('media-search').placeholder = root ? 'Search folder' : 'Search media';
  if (!root) return;
  const trail = byId('folder-trail');
  trail.replaceChildren();
  const crumb = (name,id,path,last) => {
    const link = item('button','',name);
    link.type = 'button';
    if (last) link.setAttribute('aria-current','location');
    else link.onclick = () => openLibraryFolder(id,path);
    trail.append(link);
  };
  const parts = libraryFolder.path.split('/').filter(Boolean);
  crumb('Media',null,'',false);
  crumb(root.name,root.id,'',!parts.length);
  for (let i = 0; i < parts.length; i++) crumb(parts[i],root.id,parts.slice(0,i+1).join('/'),i===parts.length-1);
}
function libraryFolders(root) {
  if (!root) return project.folders.toSorted((a,b)=>librarySort.compare(a.name,b.name)).map(folder=>({...folder,path:''}));
  const here = libraryFolder.path;
  const names = new Set();
  for (const source of project.sources.filter(source=>sourceInside(source))) {
    const parent = parentPath(source.relative_path);
    if (parent === here) continue;
    const tail = here ? parent.slice(here.length+1) : parent;
    if (tail) names.add(tail.split('/')[0]);
  }
  return [...names].sort(librarySort.compare).map(name=>({
    id:root.id,name,path:here ? here + '/' + name : name,offline:false
  }));
}
function searchForCurrentProject() {
  if (librarySearch?.projectId === projectId && librarySearch.query === libraryQuery() && librarySearch.scope === folderScope()) return librarySearch;
}
function queueLibrarySearch(delay) {
  clearTimeout(searchTimer);
  searchAbort?.abort();
  const query = libraryQuery();
  if (!query || !project) return;
  const requestedProject = projectId;
  const scope = folderScope();
  const params = new URLSearchParams({q:query});
  if (libraryFolder) {
    params.set('folder',libraryFolder.id);
    params.set('path',libraryFolder.path);
  }
  searchTimer = setTimeout(async () => {
    const request = new AbortController();
    searchAbort = request;
    const previous = searchForCurrentProject();
    try {
      const data = await api('/api/projects/' + requestedProject + '/media-search?' + params, {signal: request.signal});
      if (request.signal.aborted || requestedProject !== projectId || query !== libraryQuery() || scope !== folderScope()) return;
      librarySearch = {projectId: requestedProject, query, scope, data};
      if (JSON.stringify(data) !== JSON.stringify(previous?.data)) renderLibrary();
    } catch (error) {
      if (request.signal.aborted || requestedProject !== projectId || query !== libraryQuery() || scope !== folderScope()) return;
      librarySearch = {projectId: requestedProject, query, scope, error: error.message};
      renderLibrary();
    } finally {
      if (searchAbort === request) searchAbort = null;
    }
  }, delay);
}

function clearLibrarySkim(release = false) {
  if (librarySkim) {
    librarySkim.thumb.removeAttribute('data-skim');
    librarySkim.thumb.style.removeProperty('--source-skim-position');
    librarySkim.frame.remove();
    librarySkim = null;
    librarySkimFrame.hidden = librarySkimLabel.hidden = true;
  }
  if (release && librarySkimPlayer.hasAttribute('src')) {
    librarySkimPlayer.removeAttribute('src');
    librarySkimPlayer.load();
  }
}

function skimLibrarySource(event, thumb, asset) {
  if (event.buttons || event.pointerType === 'touch' || playingCut || waitingForAudio ||
      presentedFrame || (sourcePreview && !preview.paused)) return;
  const bounds = thumb.getBoundingClientRect();
  if (!bounds.width) return;
  const fraction = Math.max(0, Math.min(1, (event.clientX - bounds.left) / bounds.width));
  const duration = asset.duration / TICKS_PER_SECOND;
  const at = Math.max(0, Math.min(duration - .02, fraction * duration));
  if (librarySkim?.thumb !== thumb) {
    clearLibrarySkim();
    const frame = document.createElement('canvas');
    frame.hidden = true;
    frame.setAttribute('aria-hidden','true');
    thumb.prepend(frame);
    librarySkim = {thumb, frame, at};
    thumb.dataset.skim = '';
    librarySkimLabel.firstElementChild.textContent = asset.title;
    librarySkimLabel.hidden = false;
    const url = mediaUrl(asset.id);
    if (librarySkimPlayer.getAttribute('src') !== url || librarySkimPlayer.error) {
      librarySkimPlayer.src = url;
    }
  }
  librarySkim.at = at;
  thumb.style.setProperty('--source-skim-position', `${fraction * 100}%`);
  librarySkimLabel.lastElementChild.textContent = at.toFixed(2) + ' s';
  seekLibrarySkim();
}

function seekLibrarySkim() {
  const player = librarySkimPlayer;
  if (!librarySkim || player.readyState < 1 || player.seeking || !Number.isFinite(player.duration)) return;
  const at = Math.min(librarySkim.at, Math.max(0, player.duration - .02));
  if (Math.abs(player.currentTime - at) > .001) player.currentTime = at;
  else paintLibrarySkim();
}

function paintLibrarySkim() {
  const player = librarySkimPlayer;
  if (!librarySkim || player.readyState < 2 || player.seeking || !player.videoWidth ||
      librarySkim.painted === player.currentTime) return;
  librarySkim.painted = player.currentTime;
  for (const [canvas, cap] of [[librarySkimFrame,1280],[librarySkim.frame,320]]) {
    const scale = Math.min(1, cap / Math.max(player.videoWidth, player.videoHeight));
    const width = Math.max(1,Math.round(player.videoWidth * scale));
    const height = Math.max(1,Math.round(player.videoHeight * scale));
    if (canvas.width !== width) canvas.width = width;
    if (canvas.height !== height) canvas.height = height;
    canvas.getContext('2d',{alpha:false}).drawImage(player,0,0,width,height);
    canvas.hidden = false;
  }
}

librarySkimPlayer.addEventListener('loadedmetadata', seekLibrarySkim);
librarySkimPlayer.addEventListener('loadeddata', seekLibrarySkim);
librarySkimPlayer.addEventListener('seeked', () => {
  // Finish a frame before seeking again, even if the pointer has moved on.
  paintLibrarySkim();
  seekLibrarySkim();
});
librarySkimPlayer.addEventListener('error', () => clearLibrarySkim());
byId('media-list').addEventListener('scroll', () => clearLibrarySkim(), {passive:true});
window.addEventListener('blur', () => clearLibrarySkim());

function renderLibrary() {
  const missing=project.sources.filter(asset=>asset.missing_file).length;
  const locate=byId('locate-missing-folder');
  locate.hidden=missing===0;
  locate.textContent='Locate missing media ('+missing+')';
  clearLibrarySkim();
  let root = currentRoot();
  for (const id of librarySelection) {
    if (!source(id)) librarySelection.delete(id);
  }
  for (const [key,pick] of libraryFolderSelection) {
    const exists = project.folders.some(folder=>folder.id===pick.folder) &&
      (!pick.path || project.sources.some(source=>sourceInside(source,{id:pick.folder,path:pick.path})));
    if (!exists) libraryFolderSelection.delete(key);
  }
  if (!root && libraryFolder) libraryFolder = null;
  if (root) {
    while (libraryFolder.path && !project.sources.some(source=>sourceInside(source))) {
      libraryFolder.path = parentPath(libraryFolder.path);
    }
  }
  const query = libraryQuery();
  const search = query && searchForCurrentProject();
  const hits = search?.data && new Map(search.data.found.map(hit => [hit.asset_id, hit]));
  const candidates = project.sources.filter(source => query
    ? (!root || sourceInside(source))
    : root ? sourceInside(source) && parentPath(source.relative_path) === libraryFolder.path : !source.folder_id);
  const sources = candidates.filter(source => hits
    ? hits.has(source.id)
    : (source.title + ' ' + source.notes).toLowerCase().includes(query.toLowerCase()));
  const list = byId('media-list');
  list.replaceChildren();
  renderLibraryNavigation(root);
  renderDeviceFolderStatus(root);
  if (root?.offline) list.append(item('p','empty','The linked inbox folder is offline.'));
  const folders = query ? [] : libraryFolders(root);
  const order = [...folders.map(({id,path})=>({folder:id,path})), ...sources.map(({id})=>({asset:id}))];
  for (const folder of folders) {
    const tile = item('button','button media-folder','');
    tile.type = 'button';
    tile.dataset.folder = folder.id;
    tile.dataset.path = folder.path;
    tile.dataset.offline = String(folder.offline);
    const pick = {folder:folder.id,path:folder.path};
    const key = libraryPickKey(pick);
    tile.dataset.libraryPick = key;
    tile.dataset.picked = String(libraryFolderSelection.has(key));
    tile.setAttribute('aria-pressed',tile.dataset.picked);
    const count = project.sources.filter(source=>sourceInside(source,folder)).length;
    tile.title = folder.name + (folder.offline ? ' · offline' : '') + ' · Double-click to open';
    tile.append(item('span','folder-glyph','▱'),item('span','folder-name',folder.name),item('small','',String(count)));
    tile.firstChild.setAttribute('aria-hidden','true');
    tile.onclick = event => {
      pickLibraryItem(pick,event,order);
      if (event.detail===0 && !event.shiftKey && !event.metaKey && !event.ctrlKey) openLibraryFolder(folder.id,folder.path);
    };
    tile.ondblclick = event => {
      if (!event.shiftKey && !event.metaKey && !event.ctrlKey) openLibraryFolder(folder.id,folder.path);
    };
    tile.oncontextmenu = event => {
      event.preventDefault();
      openFolderActions({id:folder.id,path:folder.path});
    };
    list.append(tile);
  }
  for (const source of sources) {
    const hit = hits?.get(source.id);
    const match = hit?.said?.[0] || hit?.shows?.[0];
    const at = match?.start ?? match?.times?.[0] ?? 0;
    const row = item('div', 'source', '');
    row.dataset.librarySource = source.id;
    row.dataset.libraryPick = source.id;
    row.dataset.picked = String(librarySelection.has(source.id));
    const name = item('span', 'name', source.title);
    name.title = source.title;
    const thumb = item('div', 'source-thumb', '');
    thumb.dataset.asset = source.id;
    if (source.video && !source.missing_file) thumb.style.backgroundImage = 'url(' + sourceUrl('thumb',source.id) + ')';
    if (source.video && !source.image && !source.offline && source.duration > 0) {
      thumb.onpointermove = event => skimLibrarySource(event,thumb,source);
      thumb.onpointerleave = thumb.onpointercancel = thumb.onpointerdown = () => clearLibrarySkim();
    }
    thumb.append(item('span', 'duration-label', time(source.duration) + 's'));
    const pick = event => {
      if (event.target.closest('button')) return;
      pickLibraryItem({asset:source.id},event,order);
      previewSource(source, at);
    };
    row.onclick = pick;
    row.onkeydown = event => {
      if (event.target!==row || !['Enter',' '].includes(event.key)) return;
      event.preventDefault();
      pick(event);
    };
    row.tabIndex = 0;
    row.oncontextmenu = event => {
      event.preventDefault();
      clearLibrarySkim();
      const picked=librarySelection.has(source.id) ? project.sources.filter(asset=>librarySelection.has(asset.id)) : [source];
      const reloadable=picked.filter(asset=>asset.reloadable && !asset.offline).map(asset=>asset.id);
      sourceToReload = {projectId,asset:source.id,locate:Boolean(source.missing_file),reloadable};
      byId('source-menu-title').textContent = source.title;
      byId('source-download').hidden = !sourceDownloadUrl(source);
      byId('source-reload').hidden = !reloadable.length;
      byId('source-reload').textContent = reloadable.length>1 ? 'Reload '+reloadable.length+' Items from Disk' : 'Reload from Disk';
      byId('source-locate-inbox').hidden = !source.reloadable || !source.offline;
      byId('source-replace').hidden = source.reloadable;
      byId('source-replace').textContent = source.missing_file ? 'Locate…' : 'Replace file';
      byId('source-match-sequence').hidden = !source.video || source.image || Boolean(source.missing_file);
      byId('source-remove').textContent = librarySelection.has(source.id) ? libraryRemoveLabel() : 'Remove from project';
      byId('source-menu-dialog').showModal();
    };
    row.ondblclick = event => {
      if (!event.target.closest('button') && !event.shiftKey && !event.metaKey && !event.ctrlKey) add.click();
    };
    thumb.draggable = true;
    thumb.ondragstart = event => {
      clearLibrarySkim();
      draggedSources = librarySelection.has(source.id)
        ? project.sources.filter(asset=>librarySelection.has(asset.id)).map(asset=>asset.id)
        : [source.id];
      event.dataTransfer.setData(sourceDragType,draggedSources.join(','));
      event.dataTransfer.effectAllowed = 'copy';
    };
    const add = item('button', 'button', source.video ? '+' : 'Audio');
    add.type = 'button';
    add.setAttribute('aria-label','Add '+source.title+' to the timeline');
    add.onclick = () => addSourceAtPlayhead(source.id);
    row.append(thumb, name, add);
    if (query && source.folder_id) {
      const folder = project.folders.find(folder=>folder.id===source.folder_id);
      const place = [folder?.name,parentPath(source.relative_path)].filter(Boolean).join(' / ');
      row.append(item('span','source-folder',place));
    }
    if (source.missing_file) row.append(item('span','source-folder','Missing · '+source.missing_file));
    else if (source.offline) row.append(item('span','source-folder','Linked source offline'));
    else if (source.refresh_failed) row.append(item('span','source-folder','Reload failed · showing saved media'));
    if (match) {
      const excerpt = item('button', 'source-match', time(ticks(at)) + 's · ' + (match.words || match.tag));
      excerpt.type = 'button';
      excerpt.title = excerpt.textContent;
      excerpt.onclick = () => previewSource(source, at);
      row.append(excerpt);
    }
    list.append(row);
  }
  if (!query && !sources.length && !folders.length) {
    list.append(item('p','empty',root ? (root.offline ? 'Locate the inbox folder to scan it again.' : 'This folder has no media yet.') : 'Imported media and reviewed renders appear here.'));
  }
  else if (!sources.length && query) {
    list.append(item('p', 'empty', search?.error || (search?.data?.indexing
      ? 'Looking through media…'
      : hits ? (search.data.note || 'No matching media.') : 'Searching media…')));
  }
  byId('source-count').textContent = query
    ? (search?.data?.indexing ? 'Indexing media…' : hits ? sources.length + ' matches' : '')
    : (root ? sources.length : project.sources.length) ? (root ? sources.length : project.sources.length) + ' items' : '';
}

function paintLibrarySelection() {
  for (const tile of byId('media-list').querySelectorAll('[data-library-pick]')) {
    const key = tile.dataset.libraryPick;
    tile.dataset.picked = String(librarySelection.has(key) || libraryFolderSelection.has(key));
    if (tile.dataset.folder) tile.setAttribute('aria-pressed',tile.dataset.picked);
  }
}
const libraryPickKey = pick => pick.asset || pick.folder+'/'+pick.path;
const libraryPickCount = () => librarySelection.size + libraryFolderSelection.size;
const libraryRemoveLabel = () => libraryPickCount()>1 ? 'Remove '+libraryPickCount()+' selected items' : 'Remove from project';
function clearLibrarySelection() {
  librarySelection.clear();
  libraryFolderSelection.clear();
  libraryAnchor = null;
  paintLibrarySelection();
}
function pickLibraryItem(pick,event,order) {
  const key = libraryPickKey(pick);
  const choose = entry => entry.asset ? librarySelection.add(entry.asset) : libraryFolderSelection.set(libraryPickKey(entry),entry);
  const first = order.findIndex(entry=>libraryPickKey(entry)===libraryAnchor);
  const last = order.findIndex(entry=>libraryPickKey(entry)===key);
  if (event.shiftKey && first>=0 && last>=0) {
    librarySelection.clear();
    libraryFolderSelection.clear();
    order.slice(Math.min(first,last),Math.max(first,last)+1).forEach(choose);
  } else if (event.metaKey || event.ctrlKey) {
    if (!librarySelection.delete(key) && !libraryFolderSelection.delete(key)) choose(pick);
    libraryAnchor = key;
  } else {
    librarySelection.clear();
    libraryFolderSelection.clear();
    choose(pick);
    libraryAnchor = key;
  }
  setSelection([]);
  paintLibrarySelection();
}
function pickedMedia() {
  return {asset_ids:[...librarySelection],folders:[...libraryFolderSelection.values()]};
}

async function addSourceAtPlayhead(id) {
  const asset=source(id);
  if (!asset) return;
  const editor=projectId;
  const result=await mcp('add_clip',{asset_id:id,start:playheadTicks()/TICKS_PER_SECOND,first_free_track:true},{editor});
  const kind=asset.image ? 'image' : asset.video ? 'video' : 'audio';
  const placed=result?.added?.find(clip=>clip.kind===kind);
  if (placed && projectId===editor) selectClip(placed.id,{seek:false});
}

function messageBubble(message) {
  const bubble=item('p','bubble '+(message.from==='you' ? 'you' : 'director-msg'),'');
  if (message.text && navigator.clipboard?.writeText) bubble.oncontextmenu=event=>{
    if (event.target.closest('a') || String(window.getSelection())) return;
    openTimelineMenu(event,[['Copy Message',()=>navigator.clipboard.writeText(message.text)
      .catch(()=>status('The browser blocked clipboard access.'))]],'Message actions');
  };
  if (!message.citations?.length) {
    bubble.textContent=message.text;
    return bubble;
  }
  const letters=Array.from(message.text);
  const citations=message.citations.toSorted((a,b)=>(a.after ?? letters.length)-(b.after ?? letters.length));
  let previous=0;
  for (const citation of citations) {
    let url;
    try { url=new URL(citation.url); } catch (_) { continue; }
    if (url.protocol!=='https:' || url.username || url.password) continue;
    const end=Math.min(letters.length,Math.max(previous,citation.after ?? letters.length));
    bubble.append(document.createTextNode(letters.slice(previous,end).join('')));
    const link=item('a','director-source',citation.label || url.hostname);
    link.href=url.href;
    link.target='_blank';
    link.rel='noopener noreferrer';
    link.title=url.hostname;
    bubble.append(link);
    previous=end;
  }
  bubble.append(document.createTextNode(letters.slice(previous).join('')));
  return bubble;
}

// What a run did to the timeline, as a change list: green for additions, red for removals,
// amber for clips that were changed in place.
let undoableRun=null;
function describeChange(step) {
  const place=[step.track_id ? '' : step.track,step.after!=null ? tc(step.after) : step.before!=null ? tc(step.before) : ''].filter(Boolean).join(' \u00b7 ');
  if (step.track_id) return {kind:step.after==null ? 'remove' : step.before==null ? 'add' : 'change',name:step.label || step.track,place:'',notes:[]};
  if (step.ruler_item) {
    const name=step.ruler_item==='marker' ? 'Marker'+(step.label ? ' \u201c'+step.label+'\u201d' : '') : step.label || step.ruler_item;
    return {kind:step.after==null ? 'remove' : step.before==null ? 'add' : 'change',name,place,notes:step.before!=null && step.after!=null ? ['Moved'] : []};
  }
  const name=step.label || source(step.source)?.title || 'Clip';
  if (step.before==null) return {kind:'add',name,place,notes:[Number(time(step.after_width))+' s'],clip:step.clip};
  if (step.after==null) return {kind:'remove',name,place,notes:[Number(time(step.before_width || 0))+' s']};
  const notes=[];
  const detail=step.detail || {};
  if (step.before_width!==step.after_width) notes.push('Trimmed '+Number(time(step.before_width))+' s \u2192 '+Number(time(step.after_width))+' s');
  if (step.before!==step.after || (step.from_track && step.from_track!==step.track)) notes.push('Moved');
  if (detail.gain) notes.push('Volume '+(detail.gain[1]<0 ? '\u2212' : '+')+Math.abs(Math.round(detail.gain[1]*10)/10)+' dB');
  if (detail.fade_in) notes.push(detail.fade_in[1] ? 'Fade in' : 'Fade in removed');
  if (detail.fade_out) notes.push(detail.fade_out[1] ? 'Fade out' : 'Fade out removed');
  if (detail.grade) notes.push('Color');
  if (detail.transition) notes.push(detail.transition[1] ? 'Transition' : 'Transition removed');
  if (detail.typed) notes.push('Text');
  if (!notes.length) notes.push('Adjusted');
  return {kind:'change',name,place,notes,clip:step.clip};
}
function changeList(studio) {
  // Several steps on one clip (placed, then faded) read as one line.
  const rows=[];
  const byClip=new Map();
  for (const step of studio.changes) {
    const row=describeChange(step);
    const known=!step.track_id && !step.ruler_item && byClip.get(step.clip);
    if (!known) {
      rows.push(row);
      if (!step.track_id && !step.ruler_item) byClip.set(step.clip,row);
      continue;
    }
    if (row.kind==='remove') Object.assign(known,{kind:known.kind==='add' ? 'gone' : 'remove',notes:row.notes});
    else known.notes=[...new Set([...known.notes,...row.notes])].filter((note,_,all)=>note!=='Adjusted' || all.length===1);
  }
  for (let index=rows.length-1;index>=0;index--) if (rows[index].kind==='gone') rows.splice(index,1);
  if (!rows.length) return document.createDocumentFragment();
  const box=item('section','change-list','');
  box.setAttribute('aria-label','Timeline changes');
  const head=item('header','','');
  const count=kind=>rows.filter(row=>row.kind===kind).length;
  for (const [kind,sign,word] of [['add','+','added'],['remove','\u2212','removed'],['change','~','changed']]) {
    if (!count(kind)) continue;
    const chip=item('span','change-count '+kind,sign+count(kind));
    chip.title=count(kind)+' '+word;
    chip.setAttribute('aria-label',count(kind)+' '+word);
    head.append(chip);
  }
  if (undoableRun?.id===studio.id && undoableRun.revision===savedProject.revision && savedProject.canUndo) {
    const undo=item('button','button change-undo','Undo');
    undo.type='button';
    undo.title='Undo this whole edit';
    undo.onclick=()=>{
      undoableRun=null;
      byId('undo').click();
    };
    head.append(undo);
  }
  box.append(head);
  const list=item('ul','','');
  for (const row of rows.slice(0,8)) {
    const line=item('li',row.kind,'');
    const sign=item('b','',row.kind==='add' ? '+' : row.kind==='remove' ? '\u2212' : '~');
    sign.setAttribute('aria-hidden','true');
    const words=item('span','','');
    words.append(item('strong','',row.name),item('small','',[row.place,...row.notes].filter(Boolean).join(' \u00b7 ')));
    line.append(sign,words);
    line.setAttribute('aria-label',({add:'Added ',remove:'Removed ',change:'Changed '})[row.kind]+row.name);
    if (row.clip) {
      line.tabIndex=0;
      line.dataset.jump='true';
      const jump=()=>{
        if (!directorOwnsCut() && project.timeline.some(clip=>clip.id===row.clip)) selectClip(row.clip);
      };
      line.onclick=jump;
      line.onkeydown=event=>{ if (event.key==='Enter') jump(); };
    }
    list.append(line);
  }
  box.append(list);
  if (rows.length>8) box.append(item('p','change-more','and '+(rows.length-8)+' more'));
  return box;
}
function studioTurnDetails(studio,expanded) {
  const nodes=[];
  if (studio.changes?.length) nodes.push(changeList(studio));
  const detail=(kind,label)=>{
    const box=item('details','history-detail','');
    box.dataset.historyDetail=studio.id+'/'+kind;
    box.open=expanded.has(box.dataset.historyDetail);
    box.append(item('summary','',label));
    nodes.push(box);
    return box;
  };
  const assets=(studio.asset_ids || []).map(id=>source(id)).filter(Boolean);
  if (assets.length) {
    const strip=item('div','history-assets','');
    const used=new Set(project.timeline.map(clip=>clip.source));
    for (const asset of assets) {
      const clip=project.timeline.filter(clip=>clip.source===asset.id && clip.kind==='video')
        .toSorted((a,b)=>timelineStart(a.id)-timelineStart(b.id))[0];
      const button=item('button','button','');
      button.type='button';
      button.setAttribute('aria-label',clip ? 'Select '+asset.title+' on the timeline' : 'Preview '+asset.title);
      if (asset.video && !asset.missing_file) {
        const image=item('img','history-poster','');
        image.src=sourceUrl('thumb',asset.id);
        image.alt='';
        image.loading='lazy';
        button.append(image);
      }
      const name=item('span','history-asset-name',asset.title);
      const duration=Number(time(clip ? length(clip) : asset.duration))+' s';
      button.title=asset.title;
      button.append(name,item('span','history-asset-duration',duration));
      button.onclick=()=>{
        if (directorOwnsCut()) return status('Wait for Director to finish before previewing footage.');
        if (clip && project.timeline.some(item=>item.id===clip.id)) return selectClip(clip.id);
        const latest=source(asset.id);
        if (!latest) return;
        openLibraryFolder(latest.folder_id,latest.relative_path ? parentPath(latest.relative_path) : '');
        pickLibraryItem({asset:latest.id},{},[]);
        previewSource(latest);
      };
      const row=item('div','history-asset','');
      row.append(button);
      const url=sourceDownloadUrl(asset);
      if (url) {
        const download=item('a','button history-download','↓');
        download.href=url;
        download.download='';
        download.title='Download '+asset.title;
        download.setAttribute('aria-label',download.title);
        row.append(download);
      }
      strip.append(row);
    }
    if (studio.failed && assets.some(asset=>asset.video && !asset.image && !asset.offline &&
        !asset.missing_file && !used.has(asset.id))) {
      const button=item('button','button','Append reviewed footage');
      button.type='button';
      button.onclick=async()=>{
        const editor=projectId;
        if (directorOwnsCut()) return status('Wait for Director to finish editing this project.');
        button.disabled=true;
        try {
          const result=await post(projectUrl('/director/runs/'+studio.id+'/append-reviewed'),{});
          await refresh();
          if (projectId!==editor) return;
          const last=result.clip_ids.at(-1);
          if (last) {
            selectClip(last);
            status('Reviewed footage appended.');
          } else status('Reviewed footage is already placed or unavailable.');
        } catch(error) {
          if (projectId===editor) status(error.message);
        } finally {
          button.disabled=false;
        }
      };
      strip.append(button);
    }
    nodes.push(strip);
  }
  const shots=studio.plan?.shots || [];
  if (shots.length) {
    const seconds=shot=>Math.round((shot.durationSeconds ?? 124/24)*24)/24;
    const label=duration=>Number(duration.toFixed(1))+' s';
    const plan=detail('plan','Shot plan · '+label(shots.reduce((total,shot)=>total+seconds(shot),0)));
    if (studio.plan.continuity) plan.append(item('p','history-detail-text',studio.plan.continuity));
    const list=item('ol','history-list','');
    for (const shot of shots) {
      const entry=item('li','','');
      entry.append(item('strong','',shot.title+' · '+label(seconds(shot))),item('p','history-detail-text',shot.direction));
      list.append(entry);
    }
    plan.append(list);
  }
  if (studio.events?.length) {
    const activity=detail('activity','Run activity');
    const list=item('ol','history-list','');
    for (const step of studio.events) {
      const entry=item('li','',step.title);
      if (step.detail) entry.append(item('p','history-detail-text',step.detail));
      list.append(entry);
    }
    activity.append(list);
  }
  return nodes;
}

function bubbleList(entries,expanded) {
  if (!entries.length) return [item('p', 'empty', 'Ask for a sequence or an edit to the timeline.')];
  return entries.map(message => {
    const bubble=messageBubble(message);
    if (!message.studio) return bubble;
    const exchange=item('div','studio-exchange','');
    if (message.text) exchange.append(bubble);
    if (message.studio.failed) {
      const failure=item('p','history-failure',message.text ? 'Run ended before completion' : 'This run ended before completion.');
      exchange.append(failure);
    }
    exchange.append(...studioTurnDetails(message.studio,expanded));
    return exchange;
  });
}

let elapsedRun=null;
let studioClock=null;

function renderRunElapsed() {
  const element=byId('run-elapsed');
  const start=elapsedRun?.started_at;
  const active=elapsedRun?.status==='running';
  const end=active ? (studioClock ? studioClock.sampledAt+performance.now()-studioClock.receivedAt : Date.now())/1000
    : elapsedRun?.finished_at;
  const visible=Number.isFinite(start) && Number.isFinite(end);
  element.hidden=!visible;
  if (!visible) return;
  const seconds=Math.max(0,Math.floor(end-start));
  const display=Math.floor(seconds/60)+':'+String(seconds%60).padStart(2,'0');
  if (element.textContent===display) return;
  element.textContent=display;
  element.dateTime='PT'+seconds+'S';
  element.setAttribute('aria-label',seconds+' seconds elapsed');
}

setInterval(()=>{
  if (elapsedRun?.status==='running' && !document.hidden) renderRunElapsed();
},1000);

function renderActivity() {
  const conversation = byId('conversation');
  const runPlan = byId('run-plan');
  const expanded = new Set([...byId('director-panel').querySelectorAll('details[data-history-detail][open]')]
    .map(box=>box.dataset.historyDetail));
  const finished=state.runs.find(run=>run.project===projectId);
  if (finished && finished.id===shownRun && finished.status!=='running' && finished.applied) {
    undoableRun={id:finished.id,revision:savedProject.revision};
  }
  const messages = JSON.stringify([project.id,project.revision,project.library_revision,project.sources.length,project.messages,
    undoableRun?.revision===savedProject.revision && savedProject.canUndo ? undoableRun.id : null]);
  if (conversation.dataset.content !== messages) {
    const scroll=conversation.scrollTop;
    const follow=conversation.scrollHeight-conversation.clientHeight-scroll<30;
    conversation.dataset.content = messages;
    conversation.replaceChildren(...bubbleList(project.messages,expanded));
    conversation.scrollTop = follow ? conversation.scrollHeight : scroll;
  }
  const run = state.runs.find(run => run.project === projectId);
  elapsedRun=run;
  renderRunElapsed();
  renderRecoverableShots();
  const active=run?.status==='running';
  const plan=active && run.studio_turn;
  const planKey=plan ? JSON.stringify([plan,project.revision,project.library_revision]) : '';
  if (runPlan.dataset.content!==planKey) {
    runPlan.dataset.content=planKey;
    runPlan.replaceChildren(...(plan ? studioTurnDetails(plan,expanded) : []));
    runPlan.hidden=!runPlan.childElementCount;
  }
  const toggle=byId('toggle-director');
  toggle.dataset.busy=String(active);
  const worker={claude:'Claude',chatgpt:'ChatGPT',local:'Local'}[state.provider] || 'Director';
  const stoppable=active && !run.applied && !run.stop_requested && stoppingRun!==run.id;
  toggle.dataset.working=String(stoppable);
  toggle.dataset.worker=worker;
  toggle.textContent=stoppable ? worker : 'Director';
  toggle.setAttribute('aria-label',stoppable ? 'Stop '+worker : active ? 'Director, edit in progress' : 'Director');
  const liveLine=byId('director-live');
  const liveEvents=active ? (directorWatch?.run===run.id && directorWatch.events?.length ? directorWatch.events : run.events) : [];
  const words=String(liveEvents.at(-1) || '');
  const short=words.length>40 ? words.slice(0,40)+'\u2026' : words;
  if (liveLine.dataset.words!==short) {
    liveLine.dataset.words=short;
    liveLine.classList.remove('shown');
    clearTimeout(liveLine.timer);
    liveLine.timer=setTimeout(()=>{
      liveLine.textContent=short;
      liveLine.classList.toggle('shown',Boolean(short));
    },short && liveLine.textContent ? 100 : 0);
  }
  const runPanel = byId('run-panel');
  runPanel.hidden = !run;
  if (run) {
    if (run.id === shownRun && !active) {
      if (run.applied && run.edits?.length) {
        if (!showDirectorWork) fitCompletedDirector=true;
        else if (showDirectorWork && !previewedRuns.has(run.id)) queuedReplay=run.edits;
      }
      shownRun = null;
    }
    if (thinkingShow && (thinkingRun!==run.id || !active || run.stop_requested || stoppingRun===run.id)) {
      if (queuedReplay && thinkingRun===run.id) thinkingShow.handOver();
      else endThinking();
    }
    const events = byId('events');
    const live=directorWatch?.run===run.id ? directorWatch.events : null;
    const entries=active && live && live.length>=run.events.length ? live : run.events;
    const visible = JSON.stringify(entries);
    runPanel.dataset.active = String(active);
    const labels={running:'In progress',done:'Complete',partial:'Partially saved'};
    byId('run-status').textContent = labels[run.status] || (run.stop_requested ? 'Stopped' : 'Failed');
    byId('stop-run').hidden = !active || run.applied;
    byId('stop-run').disabled = run.stop_requested || stoppingRun===run.id;
    if (active && (run.stop_requested || stoppingRun===run.id)) byId('run-status').textContent = 'Stopping';

    if (events.dataset.content !== visible) {
      events.dataset.content = visible;
      events.replaceChildren(...entries.map(event => item('li', '', event)));
      events.scrollTop = events.scrollHeight;
    }
  }
  const busy=directorBusy();
  byId('send').disabled = busy || submittingDirector;
  byId('auto-edit').disabled = busy || !savedProject.sources.some(asset=>asset.video && !asset.offline && (asset.image || asset.duration>=TICKS_PER_SECOND));
  for (const tab of document.querySelectorAll('.provider-tab')) {
    const connected=providerProfile(tab.dataset.name);
    tab.setAttribute('aria-pressed',String(tab.dataset.name===state.provider));
    tab.dataset.connected=String(Boolean(connected));
    tab.title=connected ? providerDescription(connected) : '';
  }
  const current=providerProfile(state.provider);
  const localRecipe=state.provider==='local' && state.builtin_local;
  byId('provider-status').textContent=localRecipe ? 'Local · On this host' : current ? providerDescription(current) : 'Connect a planner to start.';
  byId('quick-mode').disabled=localRecipe;
  if (localRecipe && quick) mode(false);
  byId('duration').closest('label').hidden=localRecipe;
  byId('prompt').placeholder=localRecipe ? 'Keep the cut, make it vertical, and add the title “City Walk”.' : 'Follow a paper boat under a bridge in three connected shots.';
}

function renderRecoverableShots() {
  const list=byId('recover-renders');
  const shots=(state.recoverable_renders || []).filter(shot=>shot.project===projectId);
  const key=JSON.stringify(shots);
  if (key===list.dataset.content) return;
  list.dataset.content=key;
  list.hidden=!shots.length;
  list.replaceChildren(...shots.map(shot=>{
    const row=item('div','recover-shot','');
    row.append(item('strong','',shot.title));
    const button=item('button','button',shot.active ? 'Recovering…' : 'Recover footage');
    button.type='button';
    button.disabled=shot.active;
    button.setAttribute('aria-label',(shot.active ? 'Recovering ' : 'Recover footage for ')+shot.title);
    button.onclick=async ()=>{
      button.disabled=true;
      byId('director-error').textContent='';
      try {
        await post('/api/projects/'+shot.project+'/director/recover/'+shot.id);
        await refresh();
      } catch (error) {
        if (projectId===shot.project) byId('director-error').textContent=error.message;
      } finally {
        button.disabled=false;
      }
    };
    row.append(button);
    if (shot.issue && !shot.active) row.append(item('p','history-failure',shot.issue));
    return row;
  }));
}

const agentLinger = 12000;
const parkedAgents = new Map();
let agentActivityTimer = null;
let mcpWatch = null;
let mcpFrame = null;

function agentColor(agent) {
  if (agent.name==='Claude') return '#d97857';
  if (agent.name==='Codex' || agent.name==='ChatGPT') return '#0fa380';
  return 'hsl('+[...agent.id].reduce((h,c)=>(31*h+c.charCodeAt(0))%360,0)+' 73% 72%)';
}

function hideAgentActivity() {
  clearTimeout(agentActivityTimer);
  const activity=byId('agent-activity');
  activity.hidden=true;
  delete activity.dataset.agent;
}

function setAgentActivity(agent,label) {
  clearTimeout(agentActivityTimer);
  const activity=byId('agent-activity');
  activity.dataset.agent=agent.id;
  activity.style.setProperty('--agent-color',agentColor(agent));
  byId('agent-name').textContent=agent.name;
  byId('agent-action').textContent=label;
  activity.hidden=false;
  agentActivityTimer=setTimeout(hideAgentActivity,agentLinger);
}

function agentCursor(agent) {
  const cursor=item('span','director-cursor agent-cursor','');
  const arrow=item('span','','');
  if (window.MagicShow) arrow.append(MagicShow.arrow(agentColor(agent)));
  else arrow.textContent='➤';
  cursor.append(arrow,item('b','',agent.action ? agent.name+' \u00b7 '+agent.action : agent.name));
  cursor.dataset.agent=agent.id;
  cursor.style.setProperty('--agent-color',agentColor(agent));
  cursor.setAttribute('aria-hidden','true');
  return cursor;
}

function releaseAgent(id) {
  const parked=parkedAgents.get(id);
  if (parked) {
    clearTimeout(parked.timer);
    parked.cursor.remove();
    parkedAgents.delete(id);
  }
  return parked;
}

const cursorPosition = (x,y) => 'translate('+x+'px,'+y+'px)';

function parkAgent(agent,cursor,x,y) {
  releaseAgent(agent.id);
  x=Math.max(4,Math.min(x,innerWidth-92));
  y=Math.max(4,Math.min(y,innerHeight-42));
  cursor.style.transform=cursorPosition(x,y);
  cursor.classList.add('agent-parked');
  cursor.querySelector('b').textContent=agent.name;
  document.body.append(cursor);
  parkedAgents.set(agent.id,{cursor,x,y,timer:setTimeout(()=>releaseAgent(agent.id),agentLinger)});
}

function closeMcpWatch() {
  mcpWatch?.controller.abort();
  mcpWatch=null;
  mcpFrame=null;
  for (const id of parkedAgents.keys()) releaseAgent(id);
  hideAgentActivity();
}

function observeMcp(project) {
  if (mcpWatch?.project===project) return;
  closeMcpWatch();
  const watch={project,viewer:crypto.randomUUID(),controller:new AbortController(),seen:new Set()};
  mcpWatch=watch;
  readMcp(watch);
}

function clearMcpFrame(watch,revision) {
  if (!mcpFrame || mcpFrame.project!==watch.project) return;
  mcpFrame.committed=Number.isInteger(revision);
  mcpFrame=null;
}

async function readMcp(watch) {
  const {signal}=watch.controller;
  const current=()=>!signal.aborted && projectId===watch.project;
  let after=null;
  try {
    while (current()) {
      const cursor=after===null ? '' : '&after='+after;
      const packet=await api('/api/projects/'+watch.project+'/mcp-activity?viewer='+watch.viewer+cursor,{signal});
      if (!current()) return;
      after=packet.cursor;
      for (const event of packet.events) {
        if (!current()) return;
        await applyMcpEvent(watch,event,current);
      }
      if (current() && mcpFrame?.project===watch.project && packet.active!==mcpFrame.run) {
        clearMcpFrame(watch);
        await refresh();
      }
    }
  } catch {
    if (mcpWatch===watch) {
      closeMcpWatch();
      if (presentedFrame?.external && projectId===watch.project) renderStudio(state);
    }
  }
}

async function applyMcpEvent(watch,event,current) {
  const {agent,kind,edit,frame,revision}=event;
  if (kind==='disconnected') {
    releaseAgent(agent.id);
    if (byId('agent-activity').dataset.agent===agent.id) hideAgentActivity();
    return;
  }
  if (kind==='focus') {
    setAgentActivity(agent,event.label);
    const target=event.asset && byId('media-list').querySelector('[data-asset="'+event.asset+'"]');
    if (target) {
      const box=target.getBoundingClientRect();
      if (box.width && box.height) parkAgent(agent,agentCursor(agent),box.x+box.width/2,box.y+box.height/2);
    }
    return;
  }
  if (kind==='finish') {
    if (mcpFrame?.run===edit) {
      clearMcpFrame(watch,revision);
      await refresh();
    }
    if (current()) setAgentActivity(agent,Number.isInteger(revision) ? 'Edit applied' : 'Edit discarded');
    return;
  }
  if (watch.seen.has(edit)) return;
  const needed=kind==='preview' ? frame.base_revision : revision;
  if (savedProject.revision<needed) await refresh();
  if (!current() || savedProject.revision!==needed) return;
  if (kind==='preview') {
    mcpFrame={...frame,run:edit,project:watch.project,external:true};
    renderStudio(state);
    if (presentedFrame!==mcpFrame) return;
  }
  watch.seen.add(edit);
  if (watch.seen.size>48) watch.seen.delete(watch.seen.values().next().value);
  setAgentActivity(agent,event.label);
  await replayEdits(frame.edits,watch.controller.signal,{...agent,action:event.label});
}

function closeDirectorPreview() {
  directorWatch?.controller.abort();
  directorWatch=null;
  directorFrame=null;
}

function observeDirectorDraft(run) {
  if (!run || run.status!=='running' || run.stop_requested || run.id===stoppingRun) {
    closeDirectorPreview();
    return;
  }
  if (directorWatch?.run===run.id) return;
  closeDirectorPreview();
  const watcher={run:run.id,project:projectId,controller:new AbortController()};
  directorWatch=watcher;
  beginThinking(run);
  watchDirectorDraft(watcher);
}

async function watchDirectorDraft(watcher) {
  const {run,project:editor,controller:{signal}}=watcher;
  let after=0;
  let activity=0;
  try {
    while (!signal.aborted && projectId===editor) {
      const packet=await api('/api/projects/'+editor+'/director/draft?run='+run+'&after='+after+'&activity='+activity,{signal});
      if (signal.aborted || projectId!==editor) return;
      if (packet.activity_events) {
        activity=packet.activity_version;
        watcher.events=packet.activity_events;
        noteThinking(packet.activity_events);
        renderActivity();
      }
      for (const event of packet.frames) {
        if (signal.aborted || projectId!==editor) return;
        after=event.sequence;
        if (savedProject.revision!==event.base_revision) continue;
        if (thinkingShow && thinkingRun===run) {
          sketchDraft(event.edits);
          continue;
        }
        directorFrame={...event,run};
        if (!showDirectorWork) continue;
        renderStudio(state);
        if (presentedFrame===directorFrame) await replayDirectorEdits(event.edits,signal);
      }
      if (packet.finished) {
        await refresh();
        return;
      }
    }
  } catch (error) {
    if (!signal.aborted && directorWatch===watcher) {
      closeDirectorPreview();
      // The ordinary activity poll reconnects after a dropped preview request.
    }
  }
}

function renderCredits() {
  byId('credit-button').textContent = state.available + ' credits';
  byId('credit-summary').textContent = state.available + ' credits available';
  byId('credit-history').replaceChildren(...state.charges.map(charge =>
    item('li', '', charge.title + ' · ' + charge.amount + ' · ' + charge.state)));
  byId('credit-form').hidden = accountMode || !['localhost', '127.0.0.1', '[::1]'].includes(location.hostname);
  byId('credit-checkout').hidden = !billingPack;
  if (billingPack) {
    const price=new Intl.NumberFormat(undefined,{style:'currency',currency:billingPack.currency}).format(billingPack.amount/100);
    byId('credit-pack').textContent=billingPack.credits+' render credits · '+price+' in test mode';
  }
}

function leaveSourcePreview() {
  if (!sourcePreview) return;
  sourcePreview = null;
  byId('audio-preview-name').hidden = true;
  preview.pause();
  preview.removeAttribute('src');
  preview.hidden = true;
  byId('play-cut').disabled = !clipTotal();
}

function setSelection(ids,primary) {
  clearLibrarySkim();
  leaveSourcePreview();
  const next = new Set(ids);
  if (next.size && libraryPickCount()) clearLibrarySelection();
  captionSelection.clear();
  captionFocus = false;
  selection.clear();
  next.forEach(id => selection.add(id));
  selected = next.has(primary) ? primary : (next.has(selected) ? selected : next.values().next().value ?? null);
  updateSelection();
}
function selectClip(id,{extend = false,keep = false,seek = true} = {}) {
  const linked = linkedSelection(new Set([id])).map(clip => clip.id);
  const group = linked.length ? linked : [id];
  if (extend) {
    const next = new Set(selection);
    if (next.has(id)) group.forEach(clip => next.delete(clip));
    else group.forEach(clip => next.add(clip));
    setSelection(next,id);
  } else {
    setSelection(keep && selection.has(id) ? [...selection,...group] : group,id);
    if (seek) seekTimeline(timelineStart(id));
  }
}
function updateSelection() {
  const clip = currentClip();
  byId('remove').disabled = !libraryPickCount() && !linkedSelection(selection).length && !captionSelection.size;
  if (sourcePreview) return;
  byId('trim-controls').disabled = !clip;
  byId('split').disabled = !project.timeline.some(item => !trackFor(item)?.locked);
  const showCaptions = !!project.sequence.captions && (captionFocus || captionSelection.size > 0);
  byId('sequence-inspector').hidden = !!clip || showCaptions;
  byId('trim-controls').hidden = !clip;
  const buttons = [...document.querySelectorAll('[data-clip]')];
  const clipIndex = project.timeline.findIndex(item => item.id === selected);
  buttons.forEach(button => button.setAttribute('aria-pressed',String(selection.has(button.dataset.clip))));
  byId('move-left').disabled = clipIndex < 1;
  byId('move-right').disabled = clipIndex >= project.timeline.length - 1;
  const locked = clip && project.sequence.tracks.find(track => track.name === clipTrack(clip))?.locked;
  if (locked) byId('trim-controls').disabled = true;
  byId('empty-preview').hidden = clipTotal() > 0;
  preview.hidden = true;
  const picked=selectedClips();
  const groups=new Set(picked.map(item=>item.link || item.id));
  const primaryPicture=picked.find(item=>item.kind!=='audio') || clip;
  const selectionTitle=groups.size>1 ? picked.length+' clips' : primaryPicture ? clipTitle(primaryPicture) : 'Sequence';
  byId('preview-title').textContent = showCaptions ? (captionSelection.size > 1 ? captionSelection.size + ' captions'
    : captionSelection.size === 1 ? 'Caption' : 'Captions') : selectionTitle;
  renderCaptionInspector();
  updateCaptionButtons();
  renderPlaybackControls();
  renderTimingControls();
  renderPictureControls();
  drawComposed();
  if (!clip) return;
  byId('trim-in').value = (clip.start / TICKS_PER_SECOND).toFixed(5);
  byId('trim-out').value = (clip.end / TICKS_PER_SECOND).toFixed(5);
  byId('fade').checked = clip.fade_in;
  byId('mute').checked = clip.mute;
  if (clip.timed_audio && !clip.mute) clipAudioPlayer(clip);
}

function selectedClips() {
  return project.timeline.filter(clip=>selection.has(clip.id));
}
function inspectorAnchor(clips) {
  const unlocked=clips.filter(clip=>!trackFor(clip)?.locked);
  const choices=unlocked.length ? unlocked : clips;
  return choices.find(clip=>clip.id===selected) || choices[0];
}
const fadeGroup=clip=>clip.kind==='audio' ? 'audio' : 'picture';
const fadeClips=group=>selectedClips().filter(clip=>fadeGroup(clip)===group);
const fadeAnchor=group=>inspectorAnchor(fadeClips(group));
function fadeDuration(clip,side) {
  const stored=clip.effects['fade_'+side+'_frames'];
  const legacy=side==='in' && clip.fade_in ? ticks(1/3) : 0;
  return Math.min(length(clip),Math.max(stored,legacy))/TICKS_PER_SECOND;
}
function editFades(group,settings) {
  const clip_ids=fadeClips(group).filter(clip=>!trackFor(clip)?.locked).map(clip=>clip.id);
  if (clip_ids.length) return savePicture('set_audio',{clip_ids,...settings});
}
const fadeGroups=Object.fromEntries([...document.querySelectorAll('[data-fade-group]')].map(section=>{
  const group=section.dataset.fadeGroup;
  section.append(byId('fade-controls-template').content.cloneNode(true));
  const duration={},curve={};
  for (const side of ['in','out']) {
    duration[side]=section.querySelector('[data-fade-duration="'+side+'"]');
    curve[side]=section.querySelector('[data-fade-curve="'+side+'"]');
    duration[side].onchange=event=>{
      const input=event.target;
      if (input.value && input.reportValidity()) editFades(group,{['fade_'+side]:input.valueAsNumber});
    };
    curve[side].onchange=event=>editFades(group,{['fade_'+side+'_curve']:event.target.value});
  }
  const remove=section.querySelector('[data-remove-fades]');
  remove.onclick=()=>editFades(group,{fade_in:0,fade_out:0});
  return [group,{section,fields:section.querySelector('fieldset'),duration,curve,remove}];
}));
function renderTimingControls() {
  const audio=fadeClips('audio');
  const picture=fadeClips('picture');
  const timing=byId('timing-controls');
  timing.hidden=!audio.length && !picture.length;
  timing.disabled=timing.hidden;
  for (const [kind,controls] of Object.entries(fadeGroups)) {
    const clips=kind==='audio' ? audio : picture;
    const first=inspectorAnchor(clips);
    controls.section.hidden=!first;
    if (!first) continue;
    const label=audio.length && picture.length ? (kind==='audio' ? 'Audio Fades' : 'Video Fades') : 'Fades';
    controls.section.querySelector('summary').textContent=label;
    const unlocked=clips.filter(clip=>!trackFor(clip)?.locked);
    controls.fields.disabled=!unlocked.length;
    controls.remove.disabled=!unlocked.some(clip=>fadeDuration(clip,'in') || fadeDuration(clip,'out'));
    for (const side of ['in','out']) {
      controls.duration[side].max=length(first)/TICKS_PER_SECOND;
      controls.duration[side].value=fadeDuration(first,side);
      controls.curve[side].value=first.effects['fade_'+side+'_curve'];
    }
  }
}

function playbackSelection() {
  const group=linkedSelection(selection);
  const clips=group.filter(clip=>!clip.effects.freeze && ['video','audio'].includes(clip.kind));
  return {group,clips,anchor:clips.find(clip=>clip.id===selected) || clips[0],
    audio:group.filter(clip=>clip.kind==='audio')};
}

const rampControls=byId('speed-ramp-controls');
const rampGraph=byId('speed-ramp-graph');
const rampPointBox=byId('speed-ramp-points');
const rampModes=rampControls.querySelectorAll('[data-ramp-mode]');
const rampFields=[
  ['ramp-in-start','ramp-from','ramp-to','ramp-out-end'].map(byId),
  ['ramp-start-speed','ramp-speed','ramp-end-speed'].map(byId)
];
const rampPointLabels=['Start easing in','Finish easing in','Start easing out','Finish easing out','Middle speed'];
const rampPointSpeeds=[0,1,1,2,1];
const rampPointButtons=rampPointLabels.map((label,index)=>{
  const button=item('button','zoom-point','');
  button.type='button';
  if (index===4) button.dataset.part='middle';
  button.setAttribute('aria-description','Use left and right to change timing. Use up and down to change speed.');
  button.onpointerdown=event=>dragSpeedRamp(event,index);
  button.onkeydown=event=>nudgeSpeedRamp(event,index);
  rampPointBox.append(button);
  return button;
});
let rampMode=displayPreference('speed-ramp-mode','in');

function rampShape(clip) {
  const ramp=clip?.effects.ramp;
  if (!ramp || !['from','to','ease_in','ease_out','start_speed','speed','end_speed'].every(key=>Number.isFinite(ramp[key]))) return null;
  const start=clip.start/TICKS_PER_SECOND;
  const span=(clip.end-clip.start)/TICKS_PER_SECOND;
  const positions=[ramp.from-ramp.ease_in,ramp.from,ramp.to,ramp.to+ramp.ease_out].map(at=>(at-start)/span);
  const speeds=[ramp.start_speed,ramp.speed,ramp.end_speed].map(speed=>Math.max(rampMinimum,Math.min(rampMaximum,speed)));
  const original=clip.effects.reverse
    ? {points:positions.reverse().map(at=>1-at),speeds:speeds.reverse()}
    : {points:positions,speeds};
  const points=original.points.map(at=>Math.max(0,Math.min(1,at)));
  const [a,b,c,d]=points;
  if (b-a<=.001 && d-c<=.001) {
    return {points:[.3,.6,1,1],speeds:Array(3).fill(rampSpeedAt(original,.5)),mode:'in',original};
  }
  return {points,speeds:[rampSpeedAt(original,a),speeds[1],rampSpeedAt(original,d)],mode:d-c>.001 ? 'inOut' : 'in',original};
}

function renderPlaybackControls() {
  const {clips,anchor,audio}=playbackSelection();
  byId('playback-controls').hidden=!anchor && !audio.length;
  byId('clip-gain-row').hidden=!audio.length;
  byId('clip-speed-row').hidden=!anchor;
  byId('reset-playback').disabled=!clips.some(clip=>clip.effects.speed!==1 || clip.effects.ramp)
    && !audio.some(clip=>clip.effects.gain_db!==0);
  if (audio.length) byId('clip-gain').value=audio[0].effects.gain_db;
  if (anchor) byId('clip-speed').value=Number((anchor.effects.speed*100).toFixed(4));
  const shape=rampShape(anchor);
  byId('ramp-toggle').setAttribute('aria-pressed',String(Boolean(shape)));
  rampControls.hidden=!shape;
  if (shape) drawSpeedRamp(shape);
}

function speedRampTargets(anchor) {
  return {clip_id:anchor.id,...selectionArgs(),sticky:toggled('sticky')};
}
function speedRampArguments(shape,args) {
  if (!shape) return {...args,remove:true};
  const [start_speed,speed,end_speed]=shape.speeds;
  return {...args,points:shape.points,start_speed,speed,end_speed};
}
function saveSpeedRamp(shape,anchor=playbackSelection().anchor) {
  if (anchor) return savePicture('set_speed_ramp',speedRampArguments(shape,speedRampTargets(anchor)));
}
function changeClipSpeed(percent) {
  return savePicture('set_speed',{...selectionArgs(),speed:percent/100,sticky:toggled('sticky')});
}
byId('clip-speed').onchange=event=>{
  const input=event.target;
  if (!input.value || !input.reportValidity()) return;
  changeClipSpeed(input.valueAsNumber);
};
byId('clip-gain').onchange=event=>{
  const input=event.target;
  if (!input.value || !input.reportValidity()) return;
  const clip_ids=playbackSelection().audio.map(clip=>clip.id);
  if (clip_ids.length) savePicture('set_audio',{clip_ids,gain_db:input.valueAsNumber});
};
byId('reset-playback').onclick=()=>savePicture('set_speed',{...selectionArgs(),reset:true,sticky:toggled('sticky')});
byId('ramp-toggle').onclick=()=>{
  const {anchor}=playbackSelection();
  if (!anchor) return;
  if (rampShape(anchor)) return saveSpeedRamp(null,anchor);
  const points=rampMode==='inOut' ? [.3,.4,.6,.7] : [.3,.6,1,1];
  return saveSpeedRamp({points,speeds:[1,anchor.effects.speed===1 ? .5 : anchor.effects.speed,1]},anchor);
};
for (const button of rampModes) button.onclick=()=>{
  const shape=rampShape(playbackSelection().anchor);
  if (!shape) return;
  const requested=button.dataset.rampMode;
  rampMode=requested;
  storeDisplayPreference('speed-ramp-mode',requested);
  if (requested===shape.mode) return;
  let [inStart,inEnd,outStart,outEnd]=shape.points;
  let [start,middle,end]=shape.speeds;
  if (inStart===inEnd) {
    [inStart,inEnd]=[outStart,outEnd];
    [start,middle]=[middle,end];
  }
  if (requested==='in') return saveSpeedRamp({points:[inStart,inEnd,1,1],speeds:[start,middle,middle]});
  if (inEnd>.6) {
    inStart*=.6/inEnd;
    inEnd=.6;
  }
  const room=1-inEnd;
  return saveSpeedRamp({points:[inStart,inEnd,inEnd+room*.4,inEnd+room*.7],speeds:[start,middle,start]});
};
byId('ramp-apply').onclick=()=>{
  if (rampFields.flat().some(input=>!input.value || !input.reportValidity())) return;
  const [points,speeds]=rampFields.map(group=>group.map(input=>input.valueAsNumber/100));
  saveSpeedRamp({points,speeds});
};

const rampMinimum=.05;
const rampMaximum=100;
const rampLogRange=Math.log(rampMaximum/rampMinimum);
function rampSpeedAt(shape,at) {
  const [a,b,c,d]=shape.points;
  const [before,middle,after]=shape.speeds;
  function blend(from,to,amount) {
    return from*(to/from)**(amount*amount*(3-2*amount));
  }
  if (at<b) return at<=a ? before : blend(before,middle,(at-a)/(b-a));
  if (at>c) return at>=d ? after : blend(middle,after,(at-c)/(d-c));
  return middle;
}
function rampGraphY(speed,height) {
  return 6+(height-12)*(1-Math.log(speed/rampMinimum)/rampLogRange);
}
function rampGraphSpeed(y,height) {
  const position=Math.max(0,Math.min(1,1-(y-6)/(height-12)));
  const speed=rampMinimum*Math.exp(position*rampLogRange);
  return Math.abs(Math.log(speed))<.06 ? 1 : speed;
}
function drawSpeedRamp(shape,updateFields=true) {
  const width=rampGraph.clientWidth,height=rampGraph.clientHeight;
  for (const button of rampModes) button.setAttribute('aria-pressed',String(button.dataset.rampMode===shape.mode));
  if (updateFields) [shape.points,shape.speeds].forEach((group,i)=>group.forEach((value,j)=>{
    const input=rampFields[i][j];
    if (document.activeElement!==input) input.value=Number((value*100).toFixed(4));
  }));
  if (!width || !height) return;
  const density=window.devicePixelRatio || 1;
  rampGraph.width=Math.round(width*density);
  rampGraph.height=Math.round(height*density);
  const paint=rampGraph.getContext('2d');
  const colors=getComputedStyle(rampGraph);
  paint.scale(density,density);
  paint.strokeStyle=colors.getPropertyValue('--soft');
  paint.setLineDash([2,3]);
  paint.beginPath();
  paint.moveTo(0,rampGraphY(1,height));
  paint.lineTo(width,rampGraphY(1,height));
  paint.stroke();
  paint.setLineDash([]);
  paint.strokeStyle=colors.getPropertyValue('--primary');
  paint.lineWidth=1.5;
  paint.beginPath();
  for (let i=0;i<=80;i++) {
    const y=rampGraphY(rampSpeedAt(shape.original ?? shape,i/80),height);
    if (i===0) paint.moveTo(0,y);
    else paint.lineTo(width*i/80,y);
  }
  paint.stroke();
  const [a,b,c,d]=shape.points;
  const shown=[b-a>.001,true,shape.mode==='inOut',d-c>.001,shape.mode==='inOut'];
  for (const [index,button] of rampPointButtons.entries()) {
    button.hidden=!shown[index];
    const fraction=index===4 ? (b+c)/2 : shape.points[index];
    const speed=shape.speeds[rampPointSpeeds[index]];
    button.style.left=Math.max(6,Math.min(width-6,width*fraction))+'px';
    button.style.top=rampGraphY(speed,height)+'px';
    const label=rampPointLabels[index]+' · '+Math.round(speed*100)+'% speed at '+Math.round(fraction*100)+'% of footage';
    button.setAttribute('aria-label',label);
    button.title=label;
  }
}
new ResizeObserver(()=>{
  const shape=project && rampShape(playbackSelection().anchor);
  if (shape) drawSpeedRamp(shape,false);
}).observe(rampGraph);

function movedRampPoint(shape,index,fraction,speed) {
  const points=[...shape.points],speeds=[...shape.speeds];
  const gap=.003;
  if (index<4) {
    const minimum=index===0 ? 0 : points[index-1]+(index%2 ? gap : 0);
    const maximum=index===3 ? 1 : points[index+1]-(index%2 ? 0 : gap);
    points[index]=Math.max(Math.min(minimum,maximum),Math.min(maximum,fraction));
  }
  if (speed!==undefined) speeds[rampPointSpeeds[index]]=speed;
  return {points,speeds,mode:shape.mode};
}
function rampPointAdjustsSpeed(shape,index) {
  return index!==2 && (index!==1 || shape.mode==='in');
}

function paintRetimeGeometry() {
  const byClip=new Map(retimeProjection?.clips.map(clip=>[clip.id,clip]));
  const clips=new Map(project.timeline.map(clip=>[clip.id,clip]));
  for (const button of timelineBody.querySelectorAll('.clip[data-clip]')) {
    const id=button.dataset.clip;
    const shape=byClip.get(id);
    const wasProjected=button.hasAttribute('data-retime-projected');
    if (!shape && !wasProjected) continue;
    button.toggleAttribute('data-retime-projected',Boolean(shape));
    button.style.setProperty('--retime-left',shape ? shape.start*pxPerTick+'px' : '');
    button.style.setProperty('--retime-width',shape ? Math.max(5,shape.duration*pxPerTick)+'px' : '');
    button.querySelector('small').textContent=time(shape?.duration ?? clips.get(id)?.duration ?? 0)+'s';
  }
  for (const band of timelineBody.querySelectorAll('.retime-transition-band')) band.remove();
  if (!retimeProjection) return;
  const lanes=new Map(timelineLanes().map(lane=>[lane.dataset.track,lane]));
  for (const cut of retimeProjection.cuts) {
    const clip=clips.get(cut.incoming);
    if (!clip) continue;
    const band=item('span','transition-band retime-transition-band','');
    band.setAttribute('aria-hidden','true');
    styleTransitionBand(band,cut,trackFor(clip)?.hidden);
    lanes.get(clipTrack(clip))?.append(band);
  }
}
function paintRetimeProjection() {
  timelineBody.toggleAttribute('data-retime-preview',Boolean(retimeProjection));
  paintRetimeGeometry();
  drawRulerWidth();
  drawRulerTicks();
  drawTimelinePlayhead();
  drawTimelineBars();
  scheduleTimelineMedia();
  const duration=retimeProjection?.duration ?? clipTotal();
  byId('runtime').textContent=duration ? time(duration)+'s' : '';
  byId('total-time').textContent='/ '+tc(duration);
  const readout=byId('timecode');
  if (document.activeElement!==readout && !sourcePreview) readout.value=tc(displayedPlayhead());
}
function clearRetimeProjection(owner) {
  if (!retimeProjection || (owner && retimeProjection.owner!==owner)) return;
  retimeProjection=null;
  paintRetimeProjection();
  scheduleTimelineMedia();
}
function previewSpeedRamp(editor,args,owner) {
  let pending,request,timer,closed=false;
  const current=()=>projectId===editor && project.revision===args.revision &&
    project.library_revision===args.library_revision && !presentedFrame && !directorOwnsCut();
  function schedule() {
    if (!closed && pending && !request && timer==null) timer=setTimeout(load,24);
  }
  async function load() {
    timer=null;
    if (!current() || closed) return;
    const {shape,focus}=pending;
    pending=null;
    request=new AbortController();
    try {
      const draft=await api('/api/projects/'+editor+'/retime-preview',{
        method:'POST',data:{...speedRampArguments(shape,args),focus},signal:request.signal,
      });
      if (!closed && current()) {
        retimeProjection={...draft,owner};
        paintRetimeProjection();
      }
    } catch (_) {
      if (!closed && !pending) clearRetimeProjection(owner);
    } finally {
      request=null;
      schedule();
    }
  }
  return {
    offer(shape,focus) {pending={shape,focus};schedule();},
    stop() {closed=true;clearTimeout(timer);request?.abort();},
  };
}

function dragSpeedRamp(event,index) {
  if (event.button!==0) return;
  const {anchor}=playbackSelection();
  const shape=rampShape(anchor);
  if (!shape) return;
  const args={...speedRampTargets(anchor),revision:project.revision,library_revision:project.library_revision};
  const owner={};
  const projection=previewSpeedRamp(projectId,args,owner);
  const box=rampGraph.getBoundingClientRect();
  const showFrame=anchor.kind==='video' && !playingCut && !sourcePreview;
  event.currentTarget.focus({preventScroll:true});
  event.preventDefault();
  function moved(pointer) {
    const x=(pointer.clientX-box.left)/box.width;
    const speed=rampPointAdjustsSpeed(shape,index) ? rampGraphSpeed(pointer.clientY-box.top,box.height) : undefined;
    return movedRampPoint(shape,index,x,speed);
  }
  timelineGesture(event,{
    cancelOnEscape:true,
    update(pointer) {
      const next=moved(pointer);
      drawSpeedRamp(next,false);
      const fraction=index===4 ? (next.points[1]+next.points[2])/2 : next.points[index];
      projection.offer(next,fraction);
      if (showFrame && !playingCut) {
        const offset=anchor.effects.reverse ? 1-fraction : fraction;
        showTrimFrame({id:anchor.source,at:anchor.start+Math.round((anchor.end-anchor.start)*offset)});
      }
    },
    complete(pointer,changed) {
      if (changed) savePicture('set_speed_ramp',speedRampArguments(moved(pointer),args)).finally(()=>clearRetimeProjection(owner));
    },
    cancel(committing) {
      projection.stop();
      if (!committing) clearRetimeProjection(owner);
      else if (retimeProjection?.owner===owner) {
        retimeProjection.focus=null;
        paintRetimeProjection();
      }
      if (showFrame) {
        trimFrameTarget=undefined;
        trimFrame.hidden=true;
        byId('trim-readout').hidden=true;
      }
      const latest=project && rampShape(playbackSelection().anchor);
      if (latest) drawSpeedRamp(latest,false);
    }
  });
}
function nudgeSpeedRamp(event,index) {
  if (event.metaKey || event.ctrlKey || event.altKey ||
      !['ArrowLeft','ArrowRight','ArrowUp','ArrowDown'].includes(event.key)) return;
  event.preventDefault();
  event.stopPropagation();
  const {anchor}=playbackSelection();
  const shape=rampShape(anchor);
  if (!shape) return;
  const speed=shape.speeds[rampPointSpeeds[index]];
  const direction=event.key==='ArrowLeft' || event.key==='ArrowDown' ? -1 : 1;
  const horizontal=event.key==='ArrowLeft' || event.key==='ArrowRight';
  if (horizontal ? index===4 : !rampPointAdjustsSpeed(shape,index)) return;
  const fraction=shape.points[index]+(horizontal ? direction*(event.shiftKey ? .05 : .01) : 0);
  const newSpeed=horizontal ? undefined : Math.max(rampMinimum,Math.min(rampMaximum,speed*(event.shiftKey ? 1.2 : 1.05)**direction));
  return saveSpeedRamp(movedRampPoint(shape,index,fraction,newSpeed),anchor);
}

function fadeCurve(curve, progress) {
  const x = Math.max(0, Math.min(1, progress));
  if (curve === 'easeIn') return x ** 3;
  if (curve === 'easeOut') return 1 - (1 - x) ** 3;
  if (curve === 'smooth') return x * x * (3 - 2 * x);
  return x;
}
function clipFade(clip, elapsed, head) {
  const fx = pictureEffects(clip);
  head ??= fx.fade_in_frames;
  const first = head ? fadeCurve(fx.fade_in_curve,elapsed/head) : 1;
  const last = fx.fade_out_frames ? fadeCurve(fx.fade_out_curve,(length(clip)-elapsed)/fx.fade_out_frames) : 1;
  return Math.min(first,last);
}

function markedRange() {
  if (!rangeDraft) return project.play_range || null;
  const {in_point,out_point}=rangeDraft;
  if (in_point==null && out_point==null) return null;
  const a=in_point??0, b=out_point??clipTotal();
  return Math.abs(b-a)>=frameStep() ? [Math.min(a,b),Math.max(a,b)] : null;
}
function playRange() {
  const duration=clipTotal();
  const range=markedRange();
  if (!range) return [0,duration];
  const [first,last]=range.map(point=>Math.min(point,duration));
  return last>first ? [first,last] : [0,duration];
}

function playTimeline(notify = true, sample = null) {
  if (playingCut || waitingForAudio || !project || !clipTotal()) return;
  clearLibrarySkim();
  leaveSourcePreview();
  clearTimeout(scrubTimer);
  const skim=skimFrame;
  clearSkim(false);
  if (notify && skim!=null && shortcutPreferences.playFromSkimmer) placePlayhead(skim);
  startAudioContext();
  const [first,last]=playRange();
  if (notify && (frameAt>=clipTotal()-frameStep() || playLoop && (frameAt<first || frameAt>=last-frameStep()))) {
    placePlayhead(first);
  }
  const at = frameAt;
  const mix = audibleTimelineClips().filter(({clip}) => clip.timed_audio && at >= timelineStart(clip.id) && at < timelineStart(clip.id) + length(clip));
  const players = mix.map(({clip}) => clipAudioPlayer(clip));
  for (const player of players) if (player.error) player.load();
  const pending = players.filter(player => player.readyState === 0);
  if (!pending.length) return startTimelinePlayback(notify,sample);
  const loading = new AbortController();
  waitingForAudio = loading;
  byId('play-cut').textContent = '…';
  status('Preparing retimed audio…');
  Promise.all(pending.map(player => new Promise(resolve => {
    const finished = () => {
      player.removeEventListener('loadedmetadata', finished);
      player.removeEventListener('error', finished);
      loading.signal.removeEventListener('abort', finished);
      resolve();
    };
    player.addEventListener('loadedmetadata', finished, {once:true});
    player.addEventListener('error', finished, {once:true});
    loading.signal.addEventListener('abort', finished, {once:true});
  }))).then(() => {
    if (waitingForAudio !== loading) return;
    waitingForAudio = null;
    if (pending.some(player=>player.error)) {
      status('The retimed audio could not be prepared.');
      pauseTimeline();
      return;
    }
    status('');
    startTimelinePlayback(notify,sample);
  });
}

function startTimelinePlayback(notify,sample) {
  if (sample) {
    const {at,playing}=sample();
    placePlayhead(at);
    if (!playing) { pauseTimeline(); return; }
  }
  playingCut = true;
  playbackStart = frameAt;
  playbackOrigin = performance.now();
  byId('play-cut').textContent = '❚❚';
  if (notify) sendPlayback('play');
  advanceTimeline(playbackOrigin);
}

function pauseTimeline() {
  playingCut = false;
  if (waitingForAudio) {
    waitingForAudio.abort();
    waitingForAudio = null;
    status('');
  }
  if (playbackFrame !== undefined) cancelAnimationFrame(playbackFrame);
  playbackFrame = undefined;
  clearTimeout(scrubTimer);
  soundtrack.pause();
  pauseLayerAudio();
  clearMeters();
  for (const player of pictureLayers.values()) player.pause();
  byId('play-cut').textContent = '▶';
}

function advanceTimeline(now) {
  if (!playingCut) return;
  const [first,last] = playLoop ? playRange() : [0,clipTotal()];
  let at = playbackStart + ticks((now - playbackOrigin) / 1000);
  if (at >= last) {
    if (!playLoop) {
      pauseTimeline();
      placePlayhead(last);
      return;
    }
    at = first + (at - last) % (last - first);
    playbackStart = at;
    playbackOrigin = now;
  }
  const displayed = Math.max(first, Math.floor(at / frameStep()) * frameStep());
  if (frameAt !== displayed) placePlayhead(displayed);
  else drawComposed();
  syncLayerAudio(at);
  syncSoundtrack(at);
  updateMeters(now);
  playbackFrame = requestAnimationFrame(advanceTimeline);
}

function syncSoundtrack(at) {
  const clip = project.soundtrack && source(project.soundtrack);
  if (!clip || clip.missing_file || at >= clip.duration) {
    if (!soundtrack.paused) soundtrack.pause();
    return;
  }
  const url = mediaUrl(clip.id);
  if (soundtrack.getAttribute('src') !== url) soundtrack.src = url;
  setPlayerGain(soundtrack, 'soundtrack', 10**(-4.4/20));
  const offset = at / TICKS_PER_SECOND;
  if (soundtrack.readyState >= 1 && Math.abs(soundtrack.currentTime - offset) > 0.10) soundtrack.currentTime = offset;
  if (soundtrack.paused) soundtrack.play().catch(() => {});
}

preview.addEventListener('timeupdate', () => {
  if (sourcePreview) byId('timecode').value = tc(ticks(preview.currentTime));
});
preview.addEventListener('pause', () => {
  if (sourcePreview) byId('play-cut').textContent = '▶';
});
preview.addEventListener('play', () => {
  clearLibrarySkim();
  if (sourcePreview) byId('play-cut').textContent = '❚❚';
});
function toggleViewerPlayback() {
  if (sourcePreview) {
    if (preview.paused) preview.play().catch(() => {});
    else preview.pause();
  } else if (playingCut || waitingForAudio) {
    pauseTimeline();
    sendPlayback('pause');
    drawComposed();
  } else {
    playTimeline();
  }
}
byId('play-cut').onclick = toggleViewerPlayback;

async function save(patch) {
  if (presentedFrame || directorOwnsCut()) {
    status(presentedFrame?.external ? 'The attached agent is finishing this edit.' : 'Stop Director to edit the saved timeline.');
    return false;
  }
  const changes = {
    title: project.title, revision: project.revision, timeline: project.timeline, soundtrack: project.soundtrack, ...patch
  };
  try {
    await post(projectUrl(), changes);
    await refresh();
    status(state.autosave ? 'Saved' : 'Project updated');
    return true;
  } catch (error) {
    status(error.message);
    return false;
  }
}
function patchClip(patch) {
  return save({ timeline: project.timeline.map(clip => clip.id === selected ? { ...clip, ...patch } : clip) });
}
function reorder(from, to) {
  const timeline = [...project.timeline];
  timeline.splice(to, 0, ...timeline.splice(from, 1));
  return save({ timeline });
}
byId('trim').onclick = () => {
  const start = ticks(byId('trim-in').value);
  const end = ticks(byId('trim-out').value);
  const clip = currentClip();
  if (!clip || end <= start) return status('Enter a valid in and out point.');
  if ((clip.kind === 'video' || clip.kind === 'audio') && !clip.effects.freeze) {
    return mcp('trim_clip',{clip_id:clip.id,source_start:start / TICKS_PER_SECOND,source_end:end / TICKS_PER_SECOND});
  }
  const at = timelineStart(clip.id);
  const speed = clip.effects?.speed || 1;
  mcp('trim_clip',{clip_id:clip.id,start:(at+(start-clip.start)/speed) / TICKS_PER_SECOND,end:(at+(end-clip.start)/speed) / TICKS_PER_SECOND});
};
function splitSelection() {
  return mcp('split_clips',{...selectionArgs(),at:playheadTicks() / TICKS_PER_SECOND});
}
async function copySelection(cut = false) {
  if (!linkedSelection(selection).length) return;
  const origin = projectId;
  const args = selectionArgs();
  const data = await mcp('copy_clips',args,{quiet:true});
  if (!data) return;
  clipClipboard = data.clipboard;
  if (cut && projectId === origin) await mcp('delete_clips',args);
  else status(data.clipboard.clips.length === 1 ? 'Clip copied' : data.clipboard.clips.length + ' clips copied');
}
async function pasteSelection() {
  if (clipClipboard?.project !== projectId) return status('Copy clips from this project first.');
  const result = await mcp('paste_clips',{clipboard:clipClipboard,start:playheadTicks() / TICKS_PER_SECOND});
  if (result) setSelection(result.clips.map(clip => clip.id));
}
async function duplicateSelection() {
  if (!linkedSelection(selection).length) return;
  const result = await mcp('duplicate_clips',selectionArgs());
  if (result) setSelection(result.clips.map(clip => clip.id));
}
byId('split').onclick = splitSelection;
byId('move-left').onclick = () => {
  const index = project.timeline.findIndex(item => item.id === selected);
  if (index > 0) reorder(index, index - 1);
};
byId('move-right').onclick = () => {
  const index = project.timeline.findIndex(item => item.id === selected);
  if (index >= 0 && index < project.timeline.length - 1) reorder(index, index + 1);
};
function deletePicked(ripple = false) {
  if (libraryPickCount()) removePickedMedia(pickedMedia());
  else if (captionSelection.size) deleteCaptionSelection();
  else if (selection.size) mcp('delete_clips',{...selectionArgs(),ripple});
}
byId('remove').onclick = () => deletePicked();
byId('undo').onclick = () => mcp('history',{action:'undo'});
byId('redo').onclick = () => mcp('history',{action:'redo'});
byId('sequence-size').onchange = event => {
  const [width,height] = event.target.value.split(' × ').map(Number);
  mcp('set_sequence',{width,height});
};
for (const prop of ['width','height','fps']) byId('sequence-'+prop).onchange = event => mcp('set_sequence',{[prop]:Number(event.target.value)});
byId('fade').onchange = event => patchClip({ fade_in: event.target.checked });
byId('mute').onchange = event => patchClip({ mute: event.target.checked });
byId('soundtrack').onchange = event => save({ soundtrack: event.target.value || null });

async function switchProject(id) {
  projectId = id;
  renderStudio(state);
  try { await refresh(); } catch (error) { status(error.message); }
}
byId('project-select').onchange = event => switchProject(event.target.value);

const recentDialog=byId('recent-projects-dialog');
const recentList=byId('recent-project-list');
const recentClear=byId('recent-project-clear');
function recentProjectIds() {
  try {
    const ids=JSON.parse(displayPreference('recent-projects','[]'));
    return Array.isArray(ids) ? ids.filter(id=>state.projects.some(entry=>entry.id===id)).slice(0,10) : [];
  } catch (_) {
    return [];
  }
}
function noteRecentProject(id) {
  const ids=[id,...recentProjectIds().filter(entry=>entry!==id)].slice(0,10);
  storeDisplayPreference('recent-projects',JSON.stringify(ids));
}
function renderRecentProjects() {
  const ids=recentProjectIds();
  recentList.replaceChildren(...ids.map(id=>{
    const entry=state.projects.find(item=>item.id===id);
    const button=item('button','button',entry.title);
    button.type='button';
    if (id===projectId) button.setAttribute('aria-current','true');
    button.onclick=()=>{
      recentDialog.close();
      noteRecentProject(id);
      switchProject(id);
    };
    return button;
  }));
  byId('recent-project-empty').hidden=ids.length>0;
  recentClear.disabled=ids.length===0;
}
function openRecentProjects() {
  renderRecentProjects();
  recentDialog.showModal();
}
recentClear.onclick=()=>{
  storeDisplayPreference('recent-projects','[]');
  renderRecentProjects();
};

function projectDialog(rename) {
  editMode = rename;
  byId('project-dialog-title').textContent = rename ? 'Rename project' : 'New project';
  byId('project-name').value = rename ? project.title : '';
  byId('project-dialog').showModal();
}
byId('new-project').onclick = () => projectDialog(false);
byId('rename-project').onclick = () => projectDialog(true);
const roomDialog=byId('room-dialog');
const roomFile=byId('room-file');
const roomStatus=byId('room-status');
const revertDialog=byId('revert-project-dialog');
const revertConfirm=byId('revert-project-confirm');
const revertError=byId('revert-project-error');
const copyDialog=byId('save-as-dialog');
const copyName=byId('project-copy-name');
const copyConfirm=byId('project-copy-confirm');
const copyCancel=byId('project-copy-cancel');
const copyError=byId('project-copy-error');
let copyVersion;
let revertVersion;
let roomDocument;
let roomRead;
let roomTransfer;

function openRoomDialog() {
  byId('room-form').reset();
  roomDocument=null;
  byId('room-relink').hidden=true;
  byId('room-open').disabled=true;
  byId('room-open').textContent='Open Project';
  roomStatus.textContent='';
  roomDialog.showModal();
}
document.addEventListener('drop',event=>{
  if (!canImportDroppedMedia() || !acceptsLibraryDrop(event)) return;
  const files = [...event.dataTransfer.files];
  const projectFile = files.find(file=>/\.(room|cutplay|zip)$/i.test(file.name));
  if (!projectFile) return;
  event.preventDefault();
  event.stopPropagation();
  libraryDropArea.classList.remove('media-drop-target');
  clearMediaDropOverlay();
  openRoomDialog();
  const documentFiles = new DataTransfer();
  documentFiles.items.add(projectFile);
  roomFile.files = documentFiles.files;
  if (!/\.zip$/i.test(projectFile.name)) {
    const attachments = new DataTransfer();
    for (const file of files) {
      if (allowedFolderFile(file) || /\.cube$/i.test(file.name)) attachments.items.add(file);
    }
    byId('room-attachments').files = attachments.files;
  }
  readRoomFile();
},true);
async function saveProjectVersion() {
  if (!project || byId('file-save').disabled) return;
  const editor=projectId;
  try {
    await post('/api/projects/'+editor+'/save');
    await refresh();
    noteRecentProject(editor);
    if (projectId===editor) status('Project saved');
  } catch (error) {
    if (projectId===editor) status(error.message);
  }
}
function openRevertProject() {
  if (revertConfirm.disabled || byId('file-revert').disabled) return;
  revertVersion=savedProject;
  byId('revert-project-detail').textContent='Discard unsaved changes to “'+revertVersion.title+'” and restore the last saved version.';
  revertError.textContent='';
  revertDialog.showModal();
}
byId('revert-project-form').onsubmit=async event=>{
  event.preventDefault();
  const {id,revision,library_revision}=revertVersion;
  revertConfirm.disabled=true;
  try {
    await post('/api/projects/'+id+'/revert',{revision,library_revision});
    revertDialog.close();
    await refresh();
    if (projectId===id) status('Saved version restored');
  } catch (error) {
    if (revertDialog.open) revertError.textContent=error.message;
    else if (projectId===id) status(error.message);
  } finally {
    revertConfirm.disabled=false;
  }
};
function downloadRoom() {
  if (project) triggerBrowserDownload(projectUrl('/room.zip'));
}
function triggerBrowserDownload(url) {
  const link=item('a','','');
  link.href=url;
  link.download='';
  document.body.append(link);
  link.click();
  link.remove();
}
function sourceDownloadUrl(asset) {
  const saved=asset && savedProject?.sources.find(candidate=>candidate.id===asset.id);
  if (!saved || saved.missing_file || saved.version!==asset.version) return null;
  return projectUrl('/media/'+asset.id+'/download?'+new URLSearchParams({v:asset.version}));
}
function openSaveAs() {
  if (!project || byId('file-save-as').disabled || copyConfirm.disabled) return;
  copyVersion=savedProject;
  copyName.value=copyVersion.title+' copy';
  if (copyName.value.length>copyName.maxLength) copyName.value=copyVersion.title;
  copyError.textContent='';
  copyDialog.showModal();
  copyName.select();
}
copyDialog.addEventListener('cancel',event=>{
  if (copyConfirm.disabled) event.preventDefault();
});
byId('save-as-form').onsubmit = async event => {
  event.preventDefault();
  const title=copyName.value.trim();
  if (copyConfirm.disabled || !title) return;
  const {id,revision,library_revision,folders}=copyVersion;
  copyConfirm.disabled=copyCancel.disabled=true;
  copyError.textContent='';
  try {
    const copy=await post('/api/projects/'+id+'/copy',{title,revision,library_revision});
    const linkFailed=DeviceFolders.canSync() && await deviceMedia.copyLinks(id,copy.id,folders);
    await copyExportDestination(id,copy.id);
    copyDialog.close();
    try {
      await refresh(copy.id);
      status(linkFailed ? 'Project copy saved. Reconnect any unavailable linked folders.' : 'Project copy saved');
    } catch (error) {
      status('Project copy saved. '+error.message);
    }
  } catch (error) {
    copyError.textContent=error.message;
  } finally {
    copyConfirm.disabled=copyCancel.disabled=false;
  }
};
function canMatchSequence(clip) {
  const asset = clip?.kind === 'video' && source(clip.source);
  return asset?.video && !asset.image && !asset.missing_file;
}
for (const [id,action] of [['file-new',()=>projectDialog(false)],['file-open',openRoomDialog],['file-recent',openRecentProjects],
  ['file-rename',()=>projectDialog(true)],['file-save',saveProjectVersion],['file-save-as',openSaveAs],
  ['file-revert',openRevertProject],['file-download',downloadRoom],
  ['file-import',()=>byId('import').click()],['editor-settings',openEditorSettings],['shortcut-settings',openShortcutSettings]]) {
  const button=byId(id);
  button.onclick=()=>{button.closest('details').open=false;action();};
}
document.addEventListener('pointerdown',event=>{
  for (const menu of document.querySelectorAll('.top-actions details[open]')) {
    if (!menu.contains(event.target)) menu.open=false;
  }
});
roomDialog.addEventListener('close',()=>{roomRead=null;roomTransfer?.abort();});

const pathComponents=path=>path.normalize('NFC').split(/[\\/]/).filter(part=>part && part!=='.');
function attachmentMatcher(options) {
  const candidates=options.map(file=>({file,parts:pathComponents(file.webkitRelativePath || file.name)}));
  return paths=>{
    const targets=paths.filter(path=>typeof path==='string' && path).map(pathComponents);
    let best=0,result,ambiguous=false;
    for (const {file,parts:candidate} of candidates) {
      const score=Math.max(0,...targets.map(target=>{
        let count=0;
        while (target.at(-count-1)!=null && target.at(-count-1)===candidate.at(-count-1)) count++;
        return count;
      }));
      if (score>best) {best=score;result=file;ambiguous=false;}
      else if (score>0 && score===best && (file.size!==result.size || file.lastModified!==result.lastModified ||
          file.webkitRelativePath!==result.webkitRelativePath)) ambiguous=true;
    }
    return ambiguous ? undefined : result;
  };
}
function roomAttachments() {
  const find=attachmentMatcher([...byId('room-folder').files,...byId('room-attachments').files]);
  const attachments=[];
  const missingMedia=[],missingLooks=[];
  for (const asset of roomDocument.assets) {
    const file=find([asset.relativePath,asset.path,asset.playablePath,asset.name]);
    if (file) attachments.push(['asset:'+asset.id,file]);
    else missingMedia.push(asset.name || 'Unnamed media');
  }
  const looks=new Set(roomDocument.clips.map(clip=>clip.grade?.look).filter(look=>typeof look==='string' && /\.cube$/i.test(look)));
  for (const look of looks) {
    const file=find([look]);
    if (file) attachments.push(['look:'+look,file]);
    else missingLooks.push(pathComponents(look).at(-1));
  }
  return {attachments,missingMedia,missingLooks};
}
function updateRoomSelection() {
  const file=roomFile.files[0];
  const button=byId('room-open');
  button.disabled=true;
  button.textContent='Open Project';
  if (!file) return;
  if (/\.zip$/i.test(file.name)) {
    if (file.size>2*1024**3) {roomStatus.textContent='The project ZIP exceeds 2 GB.';return;}
    roomStatus.textContent='';
    button.disabled=false;
    return;
  }
  if (!roomDocument) return;
  const {missingMedia,missingLooks}=roomAttachments();
  if (missingLooks.length) roomStatus.textContent='Choose the missing color looks. '+missingLooks.slice(0,5).join(', ');
  else if (missingMedia.length) {
    roomStatus.textContent=missingMedia.length+' media '+(missingMedia.length===1 ? 'file is' : 'files are')+' missing. You can locate them in Media after opening.';
    button.textContent='Open with Missing Media';
  } else roomStatus.textContent='';
  button.disabled=missingLooks.length>0;
}
async function readRoomFile() {
  const file=roomFile.files[0];
  roomRead=file;
  roomDocument=null;
  roomStatus.textContent='';
  byId('room-open').disabled=true;
  byId('room-relink').hidden=true;
  if (!file || /\.zip$/i.test(file.name)) {updateRoomSelection();return;}
  try {
    if (file.size>16*1024**2) throw new Error('The project document exceeds 16 MB.');
    const data=JSON.parse(await file.text());
    if (roomRead!==file) return;
    const document=data.project || data;
    if (!Array.isArray(document.assets) || !Array.isArray(document.clips) ||
        !document.assets.every(asset=>asset && typeof asset.id==='string') ||
        !document.clips.every(clip=>clip && typeof clip==='object')) throw new Error('Choose a valid Editroom project file.');
    roomDocument=document;
    byId('room-relink').hidden=!document.assets.length && !document.clips.some(clip=>/\.cube$/i.test(clip.grade?.look || ''));
    updateRoomSelection();
  } catch (error) {
    if (roomRead===file) roomStatus.textContent=error instanceof SyntaxError ? 'Choose a valid Editroom project file.' : error.message;
  }
}
roomFile.onchange=readRoomFile;
byId('room-folder').onchange=byId('room-attachments').onchange=updateRoomSelection;
byId('room-form').onsubmit=async event=>{
  event.preventDefault();
  const file=roomFile.files[0];
  if (!file || roomTransfer) return;
  const bundle=/\.zip$/i.test(file.name);
  const form=new FormData();
  form.append(bundle?'bundle':'document',file);
  if (!bundle) {
    if (!roomDocument) return;
    const {attachments,missingLooks}=roomAttachments();
    if (missingLooks.length) {updateRoomSelection();return;}
    for (const [name,file] of attachments) form.append(name,file);
  }
  const transfer=new AbortController();
  roomTransfer=transfer;
  byId('room-open').disabled=true;
  roomStatus.textContent='Opening project…';
  try {
    const response=await fetch('/api/projects/open-room',{method:'POST',body:form,signal:transfer.signal});
    const data=await response.json().catch(()=>({}));
    if (!response.ok) throw new Error(data.error || 'The studio returned HTTP '+response.status+'.');
    await refresh(data.id);
    roomTransfer=null;
    roomDialog.close();
    status(project?.sources.some(asset=>asset.missing_file) ? 'Project opened with missing media' : 'Project opened');
  } catch (error) {
    if (!transfer.signal.aborted) {roomStatus.textContent=error.message;byId('room-open').disabled=false;}
  } finally {
    if (roomTransfer===transfer) roomTransfer=null;
  }
};
byId('project-form').onsubmit = async event => {
  event.preventDefault();
  const title = byId('project-name').value.trim();
  try {
    if (editMode) {
      if (!await save({ title })) return;
    } else {
      const created=await post('/api/projects',{title});
      await refresh(created.id);
    }
    byId('project-dialog').close();
  } catch (error) { status(error.message); }
};
const sequenceOfferDialog = byId('sequence-offer');
let displayedSequenceOffer;
let choosingSequenceFormat = false;

function renderSequenceOffer(saved, directorRunning) {
  const offer = directorRunning ? null : saved.sequence_offer;
  if (!offer) {
    sequenceOfferDialog.close();
    displayedSequenceOffer = null;
    return;
  }
  if (!sequenceOfferDialog.open && document.querySelector('dialog[open]')) return;
  const sameOffer = displayedSequenceOffer?.project === saved.id &&
    displayedSequenceOffer.asset === offer.asset && displayedSequenceOffer.version === offer.version;
  if (!sameOffer) displayedSequenceOffer = {...offer,project:saved.id};
  const name = saved.sources.find(source=>source.id===offer.asset)?.title || 'the imported video';
  const describe = ({width,height,fps}) => width + ' × ' + height + ' · ' + fps + ' fps';
  byId('sequence-offer-title').textContent = 'Match the sequence to “' + name + '”?';
  byId('sequence-offer-detail').textContent = 'Video: ' + describe(offer.format) + '. Current sequence: ' + describe(saved.sequence) + '.';
  if (!sameOffer) byId('sequence-offer-error').textContent = '';
  if (!sequenceOfferDialog.open) sequenceOfferDialog.showModal();
}

async function decideSequenceOffer(matchSource) {
  const offer = displayedSequenceOffer;
  if (!offer || choosingSequenceFormat) return;
  choosingSequenceFormat = true;
  const buttons = sequenceOfferDialog.querySelectorAll('button');
  buttons.forEach(button=>button.disabled=true);
  byId('sequence-offer-error').textContent = '';
  try {
    await post('/api/projects/' + offer.project + '/sequence-offer',
      {asset:offer.asset,version:offer.version,match_source:matchSource});
    await refresh();
  } catch (error) {
    if (displayedSequenceOffer===offer) byId('sequence-offer-error').textContent = error.message;
  } finally {
    choosingSequenceFormat = false;
    buttons.forEach(button=>button.disabled=false);
  }
}
byId('sequence-offer-form').onsubmit = event => {
  event.preventDefault();
  decideSequenceOffer(event.submitter?.value === 'match');
};
sequenceOfferDialog.addEventListener('cancel', event => {
  event.preventDefault();
  decideSequenceOffer(false);
});

async function importFiles(files,folder = null,projectKey = projectId) {
  if (directorOwnsCut(projectKey)) {
    if (projectId===projectKey) status('Stop Director to import media.');
    return [];
  }
  const imported = [];
  let failed = 0;
  for (const {file,path} of files) {
    if (projectId === projectKey) status('Importing ' + file.name + ' · ' + (imported.length + failed + 1) + '/' + files.length);
    const query = '?' + new URLSearchParams({offer_match:true,...(folder ? {folder,path} : {})});
    try {
      imported.push(await api('/api/projects/' + projectKey + '/import' + query,{method:'POST',file}));
    } catch (error) {
      failed++;
      if (projectId === projectKey && files.length === 1) status(error.message);
    }
  }
  if (projectId === projectKey) {
    await refresh();
    if (imported.length || files.length > 1) status(imported.length + ' media imported' + (failed ? ' · ' + failed + ' failed' : ''));
  }
  return imported;
}
byId('import').onchange = async event => {
  const files = Array.from(event.target.files,file=>({file}));
  event.target.value = '';
  if (files.length) await importFiles(files);
};
const allowedFolderFile = file => /\.(mp4|mov|mkv|m4a|mp3|aac|wav|webm|flac|jpe?g|png|webp|avi|mxf|aiff?|caf|ogg|opus|ogv|3g[2p]|mpg|mpeg|hevc|h264|wmv|ac3|flv|mts|m2ts|ts|vob|f4v)$/i.test(file.name);
const deviceMedia = new DeviceFolders(api,allowedFolderFile);
const projectApi = (id,suffix) => '/api/projects/' + id + suffix;

function captureDroppedMedia(transfer) {
  const items = [...transfer.items].filter(item=>item.kind==='file');
  if (!items.length) return [...transfer.files].map(file=>({file}));
  return items.map(item=>{
    const entry = item.webkitGetAsEntry?.();
    const file = entry?.isDirectory ? null : item.getAsFile();
    // File and directory handles can only be requested during the drop event.
    const handle = !entry?.isFile && item.getAsFileSystemHandle?.().catch(()=>null);
    return {file,entry,handle};
  });
}

async function readDirectoryEntries(root) {
  const files = [];
  async function visit(entry,prefix) {
    const reader = entry.createReader();
    for (;;) {
      const batch = await new Promise((resolve,reject)=>reader.readEntries(resolve,reject));
      if (!batch.length) break;
      for (const child of batch) {
        if (!DeviceFolders.allowedEntry(child.name,child.isDirectory)) continue;
        const path = prefix + child.name;
        if (child.isDirectory) await visit(child,path+'/');
        else if (child.isFile && allowedFolderFile(child)) {
          const file = await new Promise((resolve,reject)=>child.file(resolve,reject)).catch(()=>null);
          if (file) files.push({file,path});
        }
      }
    }
  }
  await visit(root,'');
  return files;
}

async function registerDeviceFolder(editor,handle,folder = null,children = []) {
  await deviceMedia.database();
  const action = folder ? {action:'bind_device',folder:folder.id} : {action:'create_device',name:handle.name,children};
  const result = await post(projectApi(editor,'/media-folders'),action);
  await deviceMedia.save(editor,result.folder.id,handle,result.binding);
  return result.folder;
}

async function uploadMediaFolder(editor,name,files) {
  if (!files.length) return null;
  const ordered = files.sort((a,b)=>DeviceFolders.comparePaths(a.path,b.path));
  const folder = await post(projectApi(editor,'/media-folders'),{action:'create_upload',name});
  const imported = await importFiles(ordered,folder.id,editor);
  return {folder,path:'',assets:imported.map(source=>source.id)};
}

async function importDroppedFolder(editor,handle,entry,folders) {
  const name = handle?.name || entry?.name;
  if (!DeviceFolders.allowedEntry(name,true)) return null;
  const granted = DeviceFolders.canSync() && handle?.kind==='directory' &&
    await handle.queryPermission({mode:'read'}).catch(()=>null)==='granted';
  if (granted) {
    let hasMedia = false;
    for await (const _ of deviceMedia.readFolder(handle)) { hasMedia=true;break; }
    if (!hasMedia) return null;
    const {linked,children} = await deviceMedia.relations(editor,folders,handle);
    const folder = linked?.folder || await registerDeviceFolder(editor,handle,null,children);
    const path = linked?.path || '';
    if (projectId===editor) status('Importing '+name+'…');
    await deviceMedia.sync(editor,folder.id,true,true,{handle,path});
    const assets = await deviceMedia.assets(editor,folder.id,path);
    return {folder,path,assets,removed:linked ? [] : children.map(entry=>entry.folder)};
  }
  const files = entry?.isDirectory ? await readDirectoryEntries(entry) : [];
  return uploadMediaFolder(editor,name,files);
}

async function importMediaDrop(transfer,editor,showFolder = false) {
  const entries = captureDroppedMedia(transfer);
  let files = [];
  const assets = [];
  const folders = new Map((state.projects.find(project=>project.id===editor)?.folders || []).map(folder=>[folder.id,folder]));
  const folderResults = [];
  const faults = [];
  let hasFolders = false;
  const flushFiles = async ()=>{
    if (!files.length) return;
    assets.push(...(await importFiles(files,null,editor)).map(source=>source.id));
    files = [];
  };
  for (const {file,entry,handle:pendingHandle} of entries) {
    const handle = await pendingHandle;
    if (handle?.kind==='directory' || entry?.isDirectory) {
      hasFolders = true;
      await flushFiles();
      try {
        const result = await importDroppedFolder(editor,handle,entry,[...folders.values()]);
        if (!result) continue;
        result.removed?.forEach(id=>folders.delete(id));
        folders.set(result.folder.id,result.folder);
        assets.push(...result.assets);
        folderResults.push(result);
      } catch (error) {
        faults.push(error.message);
      }
    } else if (file) files.push({file});
  }
  await flushFiles();
  if (projectId===editor && (folderResults.length || faults.length)) {
    await refresh();
    const opened = folderResults.find(result=>result.assets.length && folders.has(result.folder.id));
    if (showFolder && opened) openLibraryFolder(opened.folder.id,opened.path);
    status(faults[0] || assets.length + ' media imported');
  } else if (projectId===editor && hasFolders && !assets.length) {
    status('No supported media files were found in the dropped folders.');
  }
  return assets;
}

function renderDeviceFolderStatus(root) {
  const label = byId('device-folder-status');
  const message = root?.kind === 'device' ? {
    connect:'Connect the local folder to keep syncing on this device.',
    permission:'Use Sync this device to restore folder access.',
    partial:'Some files could not be synced. Saved media is available.',
    error:'Device folder sync paused. Retry from the folder menu.',
  }[deviceMedia.status(projectId,root.id)] || (!DeviceFolders.canSync() ? 'Saved media is available here. Sync from the linked device.' : '') : '';
  label.hidden = !message;
  label.textContent = message || '';
}

async function syncDeviceFolder(id,folder,manual,offerMatch = false) {
  let result;
  if (manual) status('Syncing device folder…');
  try {
    result = await deviceMedia.sync(id,folder,manual,offerMatch);
    if (id !== projectId) return;
    if (result?.changed) await refresh();
    if (manual) {
      const state = deviceMedia.status(id,folder);
      const detail = result?.uploaded || result?.renamed
        ? [result.uploaded ? result.uploaded + ' media synced' : '',result.renamed ? result.renamed + ' renamed' : ''].filter(Boolean).join(' · ')
        : state === 'connected' ? 'Device folder is up to date' : '';
      status(detail);
    }
  } catch (error) {
    if (id === projectId) status(error.message);
  }
  if (id === projectId) renderDeviceFolderStatus(currentRoot());
}

async function chooseDeviceFolder(folder = null) {
  if (!DeviceFolders.supported()) return;
  const id = projectId;
  try {
    const handle = await window.showDirectoryPicker({mode:'read',id:'editroom-library'});
    if (id !== projectId) return;
    const linked = await registerDeviceFolder(id,handle,folder);
    if (id === projectId) {
      await refresh();
      openLibraryFolder(linked.id);
    }
    await syncDeviceFolder(id,linked.id,true,!folder);
  } catch (error) {
    if (error.name !== 'AbortError') status(error.message);
  }
}

byId('link-device-folder').onclick = () => {
  byId('folder-dialog').close();
  chooseDeviceFolder();
};
byId('device-folder-bind').onclick = () => {
  const folder = project.folders.find(folder=>folder.id===folderMenuScope?.id);
  byId('folder-menu-dialog').close();
  if (folder) chooseDeviceFolder(folder);
};
byId('device-folder-sync').onclick = async () => {
  const folder = project.folders.find(folder=>folder.id===folderMenuScope?.id);
  const id = projectId;
  byId('folder-menu-dialog').close();
  if (!folder) return;
  try {
    // If Chrome needs to renew permission, request it from this user gesture.
    const access = await deviceMedia.requestAccess(id,folder.id);
    if (access && access !== 'granted') return;
    await syncDeviceFolder(id,folder.id,true);
  } catch (error) { if (id === projectId) status(error.message); }
};

let scanningDeviceFolders = false;
async function scanDeviceFolders() {
  if (scanningDeviceFolders || !DeviceFolders.canSync() || !project ||
      byId('studio').hidden || document.hidden) return;
  const id = projectId;
  scanningDeviceFolders = true;
  try {
    for (const folder of project.folders.filter(folder=>folder.kind==='device')) {
      if (id !== projectId || byId('studio').hidden) break;
      await syncDeviceFolder(id,folder.id,false);
    }
  } finally {
    scanningDeviceFolders = false;
  }
}
setInterval(scanDeviceFolders,5000);
document.addEventListener('visibilitychange',scanDeviceFolders);

byId('folder-upload').onchange = async event => {
  const selection = Array.from(event.target.files);
  event.target.value = '';
  const files = selection.filter(file => allowedFolderFile(file) && file.webkitRelativePath.split('/')
    .every((part,index,parts)=>DeviceFolders.allowedEntry(part,index<parts.length-1)))
    .map(file=>({file,path:file.webkitRelativePath.split('/').slice(1).join('/')}));
  if (!files.length) return status('No supported media files were found in that folder.');
  const name = files[0].file.webkitRelativePath.split('/')[0];
  const projectKey = projectId;
  try {
    const imported = await uploadMediaFolder(projectKey,name,files);
    if (projectId === projectKey && imported) openLibraryFolder(imported.folder.id);
  } catch (error) { status(error.message); }
};

async function openFolderDialog(target = null) {
  locateTarget = target;
  const select = byId('inbox-folder');
  const link = byId('folder-link');
  byId('folder-dialog-title').textContent = target ? 'Locate inbox folder' : 'Add media folder';
  byId('upload-folder-button').hidden = Boolean(target);
  byId('link-device-folder').hidden = Boolean(target) || !DeviceFolders.supported();
  byId('folder-error').textContent = '';
  link.textContent = target ? 'Locate' : 'Link folder';
  link.disabled = true;
  select.replaceChildren(option('Loading…',''));
  byId('folder-dialog').showModal();
  const loaded = projectId;
  try {
    const {folders} = await api(projectUrl('/media-inbox'));
    if (projectId !== loaded || !byId('folder-dialog').open) return;
    select.replaceChildren(...folders.map(folder=>option(folder.path ? folder.path.replaceAll('/',' / ') : 'Inbox',folder.path)));
    link.disabled = !folders.length;
  } catch (error) { byId('folder-error').textContent = error.message; }
}
byId('media-folders').onclick = () => openFolderDialog();
byId('folder-cancel').onclick = () => byId('folder-dialog').close();
byId('upload-folder-button').onclick = () => {
  byId('folder-dialog').close();
  byId('folder-upload').click();
};
byId('folder-form').onsubmit = async event => {
  event.preventDefault();
  const button = byId('folder-link');
  button.disabled = true;
  const loaded = projectId;
  const input = {action:locateTarget ? 'locate' : 'link',path:byId('inbox-folder').value};
  if (locateTarget) input.folder = locateTarget;
  try {
    status('Scanning the inbox…');
    const result = await post(projectUrl('/media-folders'),input);
    byId('folder-dialog').close();
    if (projectId === loaded) {
      await refresh();
      if (result.opened) openLibraryFolder(result.opened.id,result.opened.path);
      status(result.skipped.length ? 'Some files could not be imported' : 'Media folder saved');
    }
  } catch (error) {
    byId('folder-error').textContent = error.message;
    status('');
  } finally { button.disabled = false; }
};
function openFolderActions(scope = libraryFolder) {
  const root = project.folders.find(folder=>folder.id===scope?.id);
  if (!root) return;
  folderMenuScope = {...scope,editor:projectId};
  const key = libraryPickKey({folder:scope.id,path:scope.path});
  byId('folder-menu-title').textContent = scope.path ? baseName(scope.path) : root.name;
  byId('folder-remove').textContent = libraryFolderSelection.has(key) ? libraryRemoveLabel() : 'Remove from project';
  byId('folder-sync').hidden = root.kind !== 'inbox';
  byId('device-folder-sync').hidden = root.kind !== 'device' || !DeviceFolders.canSync();
  const bindDevice = byId('device-folder-bind');
  bindDevice.hidden = !DeviceFolders.supported() || (root.kind !== 'device' && (root.kind !== 'upload' || Boolean(scope.path)));
  bindDevice.textContent = root.kind === 'upload' ? 'Link original folder…' : 'Choose device folder';
  byId('folder-reload').hidden = root.kind !== 'inbox';
  byId('folder-reload').disabled = root.offline;
  byId('folder-locate').hidden = root.kind !== 'inbox';
  byId('folder-menu-dialog').showModal();
}
byId('folder-actions').onclick = () => openFolderActions();
byId('folder-menu-close').onclick = () => byId('folder-menu-dialog').close();
byId('folder-sync').onclick = async () => {
  byId('folder-menu-dialog').close();
  try {
    status('Scanning the inbox…');
    const result = await post(projectUrl('/media-folders'),{action:'sync'});
    await refresh();
    status(result.imported.length ? result.imported.length + ' new media imported' : 'Media folder scanned');
  } catch (error) { status(error.message); }
};
async function reloadLibrary(input) {
  const active = projectId;
  status('Reloading media from disk…');
  try {
    const result = await post(projectUrl('/media-folders'),{action:'reload',...input});
    if (active !== projectId) return;
    await refresh();
    const text = result.refreshed.length ? result.refreshed.length + ' sources refreshed'
      : result.imported.length ? result.imported.length + ' new files imported' : 'Media is up to date';
    status(text + (result.skipped.length ? ' · ' + result.skipped.length + ' could not be read' : ''));
  } catch (error) { if (active === projectId) status(error.message); }
}
byId('folder-reload').onclick = () => {
  const selected = folderMenuScope;
  byId('folder-menu-dialog').close();
  if (selected?.editor===projectId) reloadLibrary({folder:selected.id,path:selected.path});
};
byId('source-menu-close').onclick = () => byId('source-menu-dialog').close();
byId('source-add').onclick = () => {
  const target=sourceToReload;
  byId('source-menu-dialog').close();
  if (target?.projectId===projectId) addSourceAtPlayhead(target.asset);
};
byId('source-download').onclick = () => {
  const target=sourceToReload;
  byId('source-menu-dialog').close();
  if (target?.projectId!==projectId) return;
  const url=sourceDownloadUrl(source(target.asset));
  if (url) triggerBrowserDownload(url);
};
byId('source-remove').onclick = () => {
  const target = sourceToReload;
  byId('source-menu-dialog').close();
  if (target?.projectId!==projectId) return;
  const chosen = librarySelection.has(target.asset) ? pickedMedia() : {asset_ids:[target.asset]};
  removePickedMedia(chosen);
};
byId('source-match-sequence').onclick = () => {
  const selected = sourceToReload;
  byId('source-menu-dialog').close();
  if (selected?.projectId === projectId) mcp('match_sequence_to_clip',{asset_id:selected.asset});
};
byId('source-reload').onclick = () => {
  const selected = sourceToReload;
  byId('source-menu-dialog').close();
  if (selected?.projectId === projectId && selected.reloadable.length) {
    reloadLibrary({asset_ids:selected.reloadable});
  }
};
const inboxSourceDialog=byId('inbox-source-dialog');
let inboxSourceTarget;
byId('source-locate-inbox').onclick=async()=>{
  const target=sourceToReload;
  byId('source-menu-dialog').close();
  if (target?.projectId!==projectId) return;
  const asset=source(target.asset);
  if (!asset) return;
  inboxSourceTarget=target;
  byId('inbox-source-form').reset();
  byId('inbox-source-options').replaceChildren();
  byId('inbox-source-error').textContent='';
  byId('inbox-source-title').textContent='Locate '+asset.title;
  inboxSourceDialog.showModal();
  try {
    const {files}=await api('/api/projects/'+target.projectId+'/media-inbox?files=true');
    if (!inboxSourceDialog.open || target!==inboxSourceTarget) return;
    byId('inbox-source-options').replaceChildren(...files.map(path=>option(path,path)));
  } catch (error) {
    if (inboxSourceDialog.open && target===inboxSourceTarget) byId('inbox-source-error').textContent=error.message;
  }
};
byId('inbox-source-form').onsubmit=async event=>{
  event.preventDefault();
  const target=inboxSourceTarget;
  const path=byId('inbox-source-path').value.trim();
  if (!path || target?.projectId!==projectId) return;
  const button=byId('inbox-source-submit');
  button.disabled=true;
  byId('inbox-source-error').textContent='';
  try {
    const {located}=await post('/api/projects/'+target.projectId+'/media-folders',{action:'locate_source',asset:target.asset,path});
    if (projectId!==target.projectId) return;
    await refresh();
    inboxSourceDialog.close();
    status(located.length===1 ? 'Source located' : located.length+' sources located');
  } catch (error) {
    if (inboxSourceDialog.open && target===inboxSourceTarget) byId('inbox-source-error').textContent=error.message;
  } finally { button.disabled=false; }
};
byId('source-replace').onclick = () => {
  byId('source-menu-dialog').close();
  byId('replace-media').click();
};
function replaceSourceFile(target,file) {
  const query=new URLSearchParams({replace:target.asset});
  if (target.locate) query.set('locate','true');
  return api('/api/projects/'+target.projectId+'/import?'+query,{method:'POST',file});
}
byId('replace-media').onchange = async event => {
  const selected = sourceToReload;
  const file = event.target.files[0];
  event.target.value = '';
  if (!selected || !file) return;
  status(selected.locate ? 'Locating source media…' : 'Replacing source media…');
  try {
    await replaceSourceFile(selected,file);
    if (selected.projectId === projectId) {
      await refresh();
      status(selected.locate ? 'Source located' : 'Source replaced');
    }
  } catch (error) { if (selected.projectId===projectId) status(error.message); }
};
byId('locate-missing-folder').onclick = () => byId('missing-media-folder').click();
byId('missing-media-folder').onchange = async event => {
  const editor=projectId;
  const files=[...event.target.files];
  event.target.value='';
  if (!files.length) return;
  const missing=project.sources.filter(asset=>asset.missing_file);
  const find=attachmentMatcher(files);
  const matches=missing.map(asset=>({asset,file:find([asset.relative_path,asset.missing_file])}));
  const uses=new Map();
  for (const {file} of matches) if (file) uses.set(file,(uses.get(file)||0)+1);
  const pairs=matches.filter(({file})=>uses.get(file)===1);
  if (!pairs.length) return status('No unique matches in that folder. Use Locate on an individual source.');
  const button=byId('locate-missing-folder');
  button.disabled=true;
  let restored=0;
  let failed=0;
  try {
    for (const {asset,file} of pairs) {
      if (projectId!==editor) break;
      status('Locating media '+(restored+failed+1)+' of '+pairs.length+'…');
      try {
        await replaceSourceFile({projectId:editor,asset:asset.id,locate:true},file);
        restored++;
      } catch (_) { failed++; }
    }
    if (projectId===editor) {
      await refresh();
      const remaining=project.sources.filter(asset=>asset.missing_file).length;
      status(restored+' '+(restored===1 ? 'source' : 'sources')+' located'+
        (remaining ? ' · '+remaining+' still missing' : '')+(failed ? ' · '+failed+' uploads failed' : ''));
    }
  } catch (error) {
    if (projectId===editor) status(error.message);
  } finally {
    button.disabled=false;
  }
};
byId('folder-locate').onclick = () => {
  const folder = project.folders.find(folder=>folder.id===folderMenuScope?.id);
  if (!folder) return;
  byId('folder-menu-dialog').close();
  openFolderDialog(folder.id);
};
byId('folder-remove').onclick = () => {
  const scope = folderMenuScope;
  byId('folder-menu-dialog').close();
  if (scope?.editor!==projectId) return;
  const target = {folder:scope.id,path:scope.path};
  const chosen = libraryFolderSelection.has(libraryPickKey(target)) ? pickedMedia() : {folders:[target]};
  removePickedMedia(chosen);
};

async function removePickedMedia(chosen) {
  if (planningLibraryRemoval || pendingLibraryRemoval) return;
  planningLibraryRemoval = true;
  const editor = projectId;
  try {
    const plan = await mcp('remove_media',{...chosen,preview:true},{quiet:true,editor});
    if (!plan || editor!==projectId) return;
    const removal = {editor,chosen,plan};
    if (!plan.clips && !plan.soundtrack) {
      await applyLibraryRemoval(removal);
      return;
    }
    pendingLibraryRemoval = removal;
    byId('folder-remove-title').textContent = plan.items.length===1
      ? 'Remove “'+plan.items[0]+'”?' : 'Remove '+plan.items.length+' media items?';
    const impacts = [];
    if (plan.clips) impacts.push(plan.clips + (plan.clips===1 ? ' timeline clip uses' : ' timeline clips use') + ' this media');
    if (plan.soundtrack) impacts.push('the project soundtrack uses this media');
    const detail = impacts[0][0].toUpperCase()+impacts.join(', and ').slice(1);
    byId('folder-remove-detail').textContent = detail + '. These uses leave the cut with the media. Undo restores them.';
    byId('folder-remove-dialog').showModal();
  } finally {
    planningLibraryRemoval = false;
  }
}

async function applyLibraryRemoval({editor,chosen,plan}) {
  if (editor!==projectId) return;
  const result = await mcp('remove_media',{
    ...chosen,revision:plan.revision,library_revision:plan.library_revision
  },{editor});
  if (result && editor===projectId) {
    updateSelection();
    status('Media removed from project');
  }
}
byId('folder-remove-cancel').onclick = () => byId('folder-remove-dialog').close();
byId('folder-remove-dialog').addEventListener('close',()=>{pendingLibraryRemoval=null;});
byId('folder-remove-form').onsubmit = async event => {
  event.preventDefault();
  const removal = pendingLibraryRemoval;
  byId('folder-remove-dialog').close();
  if (removal) await applyLibraryRemoval(removal);
};
const exportDialog=byId('export-dialog');
const exportCanvas=byId('export-preview-picture');
const exportCrop=byId('export-preview-crop');
const exportSurface={canvas:exportCanvas,paint:exportCanvas.getContext('2d',{alpha:false}),maxWidth:960,maxHeight:540};
const exportRatio={vertical:9/16,square:1,portrait:4/5};
let exportPreview;
const exportLocation=byId('export-location');
const exportSubmit=byId('export-submit');
const exportReplace=byId('export-replace');
const canChooseExportFolder=()=>window.isSecureContext && Boolean(window.indexedDB) &&
  typeof window.showDirectoryPicker==='function';
const exportLocationKey=editor=>'export-destination.'+editor;
const exportFolderKey=editor=>'export:'+editor;
const exportStem=name=>name.trim().replace(/[\\/:"*?<>|]/g,'_');
const exportSuffix=format=>format==='audio' ? '.wav' : '.mp4';

function clearExportReplacement() {
  if (exportPreview) exportPreview.replacement=null;
  exportReplace.hidden=true;
  exportSubmit.textContent='Export';
}
function exportError(message) {
  const error=byId('export-error');
  error.textContent=message;
  error.hidden=!message;
}
function renderExportLocation(sheet) {
  if (exportPreview!==sheet) return;
  const options=[option('Browser downloads','downloads')];
  if (sheet.folder) options.push(option(sheet.folder.name,'folder'));
  if (canChooseExportFolder()) options.push(option('Choose a folder…','pick-folder'));
  if (typeof window.showSaveFilePicker==='function') options.push(option('Choose a file when exporting…','choose'));
  exportLocation.replaceChildren(...options);
  exportLocation.value=sheet.destination;
  if (!exportLocation.value) exportLocation.value=sheet.destination='downloads';
  exportLocation.disabled=exportSubmit.disabled=sheet.destinationBusy;
}
function setExportDestinationBusy(sheet,busy) {
  sheet.destinationBusy=busy;
  if (exportPreview===sheet) exportLocation.disabled=exportSubmit.disabled=busy;
}
async function restoreExportDestination(sheet,editor) {
  const saved=displayPreference(exportLocationKey(editor),'downloads');
  sheet.destination=saved==='choose' ? saved : 'downloads';
  if (canChooseExportFolder() && saved==='folder') sheet.destinationBusy=true;
  renderExportLocation(sheet);
  if (!canChooseExportFolder()) return;
  try {
    const folder=await deviceMedia.local(exportFolderKey(editor));
    if (exportPreview!==sheet || sheet.folder) return;
    if (!folder) {
      if (saved==='folder') exportError('Choose the export folder again to save to it.');
      return;
    }
    sheet.folder=folder;
    if (saved==='folder') sheet.destination=saved;
  } catch (_) {
    if (saved==='folder' && exportPreview===sheet) exportError('Choose the export folder again to save to it.');
  } finally {
    if (saved==='folder') sheet.destinationBusy=false;
    renderExportLocation(sheet);
  }
}
async function copyExportDestination(from,to) {
  const saved=displayPreference(exportLocationKey(from),'downloads');
  if (saved==='folder') {
    if (!canChooseExportFolder()) return;
    try {
      const folder=await deviceMedia.local(exportFolderKey(from));
      if (!folder) return;
      await deviceMedia.local(exportFolderKey(to),folder);
    } catch (_) { return; }
  }
  storeDisplayPreference(exportLocationKey(to),saved);
}
exportLocation.onchange=async()=>{
  const sheet=exportPreview;
  if (!sheet) return;
  const choice=exportLocation.value;
  clearExportReplacement();
  exportError('');
  if (choice!=='pick-folder') {
    sheet.destination=choice;
    return;
  }
  setExportDestinationBusy(sheet,true);
  try {
    const folder=await window.showDirectoryPicker({id:'editroom-exports',mode:'readwrite'});
    if (exportPreview!==sheet) return;
    sheet.folder=folder;
    sheet.destination='folder';
  } catch (error) {
    if (exportPreview===sheet && error.name!=='AbortError') exportError(error.message);
  } finally {
    sheet.destinationBusy=false;
    renderExportLocation(sheet);
  }
};
byId('export-name').addEventListener('input',clearExportReplacement);
byId('export-format').addEventListener('change',clearExportReplacement);

async function findExportFile(folder,filename) {
  try {
    const handle=await folder.getFileHandle(filename);
    return {handle,file:await handle.getFile()};
  } catch (error) {
    if (error.name==='NotFoundError') return null;
    throw error;
  }
}
function sameExportFile(left,right) {
  return left===right || Boolean(left && right && left.name===right.name &&
    left.size===right.size && left.lastModified===right.lastModified);
}
async function prepareExportDestination(sheet,editor,data) {
  const suffix=exportSuffix(data.format);
  const filename=data.name+suffix;
  if (sheet.destination==='folder') {
    const folder=sheet.folder;
    const permission=await folder.requestPermission({mode:'readwrite'});
    if (permission!=='granted') throw new Error('Allow this browser to save to the folder or choose browser downloads.');
    const found=await findExportFile(folder,filename);
    if (exportPreview!==sheet) return null;
    const previous=sheet.replacement;
    if (found && (previous?.filename!==filename || !sameExportFile(previous.file,found.file))) {
      sheet.replacement={filename,file:found.file};
      exportReplace.textContent='“'+filename+'” already exists in '+folder.name+'. Replace it with this export?';
      exportReplace.hidden=false;
      exportSubmit.textContent='Replace and Export';
      return null;
    }
    return {editor,folder,filename,original:found?.file ?? null};
  }
  if (sheet.destination==='choose') {
    const audio=data.format==='audio';
    const saveTo=await window.showSaveFilePicker({id:'editroom-exports',suggestedName:filename,
      types:[{description:audio ? 'WAV audio' : 'MP4 video',accept:{[audio ? 'audio/wav' : 'video/mp4']:[suffix]}}]});
    const chosen=saveTo.name;
    if (chosen.toLowerCase().endsWith(suffix)) {
      const stem=exportStem(chosen.slice(0,-suffix.length));
      if (stem && Array.from(stem).length<=100) data.name=stem;
    }
    return {editor,saveTo};
  }
  return {editor};
}
async function rememberExportDestination(sheet,editor) {
  if (sheet.destination==='folder') {
    try { await deviceMedia.local(exportFolderKey(editor),sheet.folder); }
    catch (_) { return 'EditCastle could not remember this folder.'; }
  }
  storeDisplayPreference(exportLocationKey(editor),sheet.destination);
  return '';
}

function exportDimensions(height,shape) {
  const seq=project.sequence;
  const even=value=>Math.max(2,Math.round(value/2)*2);
  if (shape==='original') {
    const size=height ?? seq.height;
    return [even(size*seq.width/seq.height),even(size)];
  }
  const size=height ?? Math.min(seq.width,seq.height);
  return [even(size),even(size/exportRatio[shape])];
}

function exportWindow(width,height,output,reframe) {
  const ratio=output[0]/output[1];
  const w=Math.min(width,height*ratio), h=Math.min(height,width/ratio);
  return {width:w,height:h,
    x:Math.max(0,Math.min(width-w,width*reframe.x-w/2)),
    y:Math.max(0,Math.min(height-h,height*reframe.y-h/2))};
}

function exportRange() {
  const total=clipTotal();
  const range=markedRange()?.map(at=>Math.min(at,total));
  return byId('export-range').value==='marked' && range && range[1]>range[0] ? range : [0,total];
}

function exportHeight() {
  const resolution=byId('export-resolution').value;
  if (resolution==='source') return null;
  if (resolution==='custom') {
    const field=byId('export-height');
    return field.checkValidity() ? Number(field.value)||null : null;
  }
  const seq=project.sequence;
  return byId('export-shape').value==='original' && seq.height>seq.width
    ? Math.round(Number(resolution)*seq.height/seq.width) : Number(resolution);
}

function exportSettings() {
  const format=byId('export-format').value;
  const video=format==='video';
  return {
    name:byId('export-name').value.trim(),
    format,shape:video ? byId('export-shape').value : 'original',
    height:video ? exportHeight() : null,codec:byId('export-codec').value,
    quality:byId('export-quality').value,
    include_audio:!video || byId('export-audio').checked,
    loudness:byId('export-loudness').value ? Number(byId('export-loudness').value) : null,
    range_only:byId('export-range').value==='marked',
    reframe_x:exportPreview.reframe.x,reframe_y:exportPreview.reframe.y
  };
}

function updateExportResolutions() {
  const field=byId('export-resolution');
  const keep=field.value || 'source';
  const seq=project.sequence;
  const shape=byId('export-shape').value;
  const label=(name,output)=>name+' ('+output.join(' × ')+')';
  field.replaceChildren(option(label('Source',exportDimensions(null,shape)),'source'),
    ...[['4K',2160],['1440p',1440],['1080p',1080],['720p',720]].map(([name,short])=>{
      const height=shape==='original' && seq.height>seq.width ? Math.round(short*seq.height/seq.width) : short;
      const row=option(label(name,exportDimensions(height,shape)),String(short));
      row.disabled=height>4320;
      return row;
    }),option('Custom','custom'));
  field.value=keep;
  if (!field.value || field.selectedOptions[0].disabled) field.value='source';
}

function refreshExportSettings() {
  if (!exportPreview) return;
  const data=exportSettings();
  const video=data.format==='video';
  byId('export-extension').textContent=video ? '.mp4' : '.wav';
  byId('export-video-fields').hidden=!video;
  byId('export-loudness').disabled=!data.include_audio;
  for (const row of exportDialog.querySelectorAll('[data-custom-resolution]')) row.hidden=byId('export-resolution').value!=='custom';
  byId('export-height').disabled=!video || byId('export-resolution').value!=='custom';
  byId('export-preview-picture').hidden=!video;
  byId('export-preview-wave').hidden=video;
  if (!video) for (const layer of exportPreview.layers.values()) layer.pause();
  if (!data.include_audio && exportPreview.playing) {
    pauseLayerAudio();
    soundtrack.pause();
  }
  const [first,last]=exportRange();
  const size=exportDimensions(data.height,data.shape);
  const fps=project.sequence.fps;
  const pixels=size[0]*size[1], web=data.quality==='web';
  const videoBitrate=pixels*Math.min(fps,60)*(web ? .085 : .2)*(data.codec==='hevc' ? .62 : 1);
  const bitrate=video ? videoBitrate+(data.include_audio ? (web ? 160000 : 256000) : 0) : 48000*24*2;
  const megabytes=bitrate*(last-first)/TICKS_PER_SECOND/8000000;
  const estimate=megabytes>=1000 ? '~'+(megabytes/1000).toFixed(1)+' GB' : '~'+Math.round(Math.max(1,megabytes))+' MB';
  const details=video ? [size.join(' × '),fps+' fps',estimate] : ['48 kHz · 24-bit',estimate];
  byId('export-summary').replaceChildren(...details.map(text=>item('span','',text)));
  exportPreview.at=Math.min(last,Math.max(first,exportPreview.at));
  drawExportPreview();
}

function drawExportCrop() {
  if (!exportPreview) return;
  const preview=exportPreview;
  exportCrop.hidden=!preview.crop;
  if (!preview.crop) return;
  const {width,height}=exportCanvas;
  const box=byId('export-preview-stage').getBoundingClientRect();
  const scale=Math.min(box.width/width,box.height/height);
  preview.pictureSize={width:width*scale,height:height*scale};
  const {x,y,width:w,height:h}=preview.crop;
  Object.assign(exportCrop.style,{
    left:(box.width-width*scale)/2+x*scale+'px',
    top:(box.height-height*scale)/2+y*scale+'px',
    width:w*scale+'px',height:h*scale+'px'
  });
}

function drawExportPreview() {
  const current=exportPreview;
  if (!current || !exportDialog.open) return;
  const [first,last]=exportRange();
  const displayed=Math.max(first,Math.min(last-frameStep(),Math.floor(current.at/frameStep())*frameStep()));
  const scrub=byId('export-preview-scrub');
  scrub.max=String(Math.max(1,last-first));
  scrub.step=String(frameStep());
  scrub.value=String(displayed-first);
  byId('export-preview-time').textContent=tc(displayed-first);
  if (byId('export-format').value==='video' && last>first) {
    const output=exportDimensions(exportHeight(),byId('export-shape').value);
    const ratio=project.sequence.width/project.sequence.height;
    current.crop=drawSequence(exportSurface,current.layers,drawExportPreview,displayed,current.playing,
      Math.abs(ratio-output[0]/output[1])>.01 ? {output,reframe:current.reframe} : null,output);
  } else {
    current.crop=null;
  }
  drawExportCrop();
}

function pauseExportPreview() {
  if (!exportPreview) return;
  exportPreview.playing=false;
  cancelAnimationFrame(exportPreview.animation);
  for (const layer of exportPreview.layers.values()) layer.pause();
  pauseLayerAudio();
  soundtrack.pause();
  const play=byId('export-preview-play');
  play.textContent='▶';
  play.setAttribute('aria-label','Play export preview');
}

function advanceExportPreview(now) {
  const current=exportPreview;
  if (!current?.playing) return;
  const [first,last]=exportRange();
  current.at=current.start+ticks((now-current.origin)/1000);
  if (current.at>=last) {
    pauseExportPreview();
    current.at=first;
    drawExportPreview();
    return;
  }
  drawExportPreview();
  if (byId('export-format').value==='audio' || byId('export-audio').checked) {
    syncLayerAudio(current.at);
    syncSoundtrack(current.at);
  }
  current.animation=requestAnimationFrame(advanceExportPreview);
}

byId('export-preview-play').onclick = () => {
  const current=exportPreview;
  if (!current) return;
  if (current.playing) {
    pauseExportPreview();
    drawExportPreview();
    return;
  }
  const [start,end]=exportRange();
  if (end<=start) return;
  if (current.at>=end-frameStep()) current.at=start;
  startAudioContext();
  current.playing=true;
  current.start=current.at;
  current.origin=performance.now();
  const play=byId('export-preview-play');
  play.textContent='❚❚';
  play.setAttribute('aria-label','Pause export preview');
  advanceExportPreview(current.origin);
};

byId('export-preview-scrub').oninput = event => {
  if (!exportPreview) return;
  exportPreview.at=exportRange()[0]+Number(event.target.value);
  exportPreview.start=exportPreview.at;
  exportPreview.origin=performance.now();
  drawExportPreview();
};

function moveExportCrop(x,y) {
  const current=exportPreview;
  if (!current?.crop) return;
  const halfX=current.crop.width/exportCanvas.width/2;
  const halfY=current.crop.height/exportCanvas.height/2;
  current.reframe={
    x:Math.min(1-halfX,Math.max(halfX,x)),
    y:Math.min(1-halfY,Math.max(halfY,y))
  };
  drawExportPreview();
}

exportCrop.onpointerdown = event => {
  if (event.button!==0 || !exportPreview?.pictureSize) return;
  const current=exportPreview;
  current.drag={id:event.pointerId,x:event.clientX,y:event.clientY,reframe:{...current.reframe}};
  exportCrop.setPointerCapture(event.pointerId);
};
exportCrop.onpointermove = event => {
  const drag=exportPreview?.drag;
  if (drag?.id!==event.pointerId) return;
  const {width,height}=exportPreview.pictureSize;
  moveExportCrop(drag.reframe.x+(event.clientX-drag.x)/width,drag.reframe.y+(event.clientY-drag.y)/height);
};
exportCrop.onlostpointercapture = () => { if (exportPreview) exportPreview.drag=null; };
exportCrop.onkeydown = event => {
  const directions={ArrowLeft:[-1,0],ArrowRight:[1,0],ArrowUp:[0,-1],ArrowDown:[0,1]};
  const direction=directions[event.key];
  if (!direction || !exportPreview) return;
  event.preventDefault();
  event.stopPropagation();
  const offset=event.shiftKey ? .1 : .02;
  moveExportCrop(exportPreview.reframe.x+direction[0]*offset,exportPreview.reframe.y+direction[1]*offset);
};

new ResizeObserver(drawExportCrop).observe(byId('export-preview-stage'));

byId('export-shape').onchange = () => {
  exportPreview.reframe={x:.5,y:.5};
  updateExportResolutions();
  refreshExportSettings();
};
byId('export-range').onchange = () => {
  pauseExportPreview();
  exportPreview.at=exportRange()[0];
  refreshExportSettings();
};
for (const name of ['format','resolution','height','codec','quality','audio','loudness']) {
  byId('export-'+name).addEventListener(name==='height' ? 'input' : 'change',refreshExportSettings);
}
byId('export').onclick = () => {
  const playing=playingCut || waitingForAudio;
  if (playing) pauseTimeline();
  preview.pause();
  const total=clipTotal();
  const range=markedRange()?.map(at=>Math.min(total,at));
  const marked=range && range[1]>range[0];
  const select=byId('export-range');
  select.options[1].disabled=!marked;
  select.options[1].textContent=marked ? 'In to out · '+time(range[1]-range[0])+' s' : 'In to out';
  select.value=marked ? 'marked' : 'whole';
  byId('export-name').value=project.export_name || displayPreference('export-name.'+projectId,project.title);
  exportError('');
  exportPreview={layers:new Map(),reframe:{x:.5,y:.5},at:marked ? range[0] : Math.min(frameAt,Math.max(0,total-frameStep())),
    destination:'downloads',folder:null,destinationBusy:false,replacement:null};
  clearExportReplacement();
  restoreExportDestination(exportPreview,projectId);
  updateExportResolutions();
  exportDialog.showModal();
  refreshExportSettings();
};
exportDialog.addEventListener('close',()=>{
  if (!exportPreview) return;
  pauseExportPreview();
  for (const layer of exportPreview.layers.values()) {
    layer.removeAttribute('src');
    layer.load();
  }
  exportPreview=null;
});

byId('export-form').onsubmit = async event => {
  event.preventDefault();
  const sheet=exportPreview;
  if (!sheet || sheet.destinationBusy || submittingExport) return;
  const data=exportSettings();
  const editor=projectId;
  const typedName=data.name;
  data.name=exportStem(data.name);
  data.remember_name=true;
  if (!data.name) {
    byId('export-name').focus();
    exportError('Enter a filename.');
    return;
  }
  let destination;
  exportError('');
  setExportDestinationBusy(sheet,true);
  try {
    destination=await prepareExportDestination(sheet,editor,data);
  } catch (error) {
    if (error.name!=='AbortError' && exportPreview===sheet) exportError(error.message);
  } finally {
    setExportDestinationBusy(sheet,false);
  }
  if (!destination || !exportDialog.open || exportPreview!==sheet || projectId!==editor ||
      byId('export-name').value.trim()!==typedName || byId('export-format').value!==data.format) return;
  if (playingCut || waitingForAudio) pauseTimeline();
  pauseExportPreview();
  exportDialog.close();
  submittingExport=true;
  byId('export').disabled=true;
  try {
    const job=await mcp('export',data,{quiet:true,editor});
    if (job) {
      pendingExportFiles.set(job.id,destination);
      exportQueueVersion++;
      updateExportQueue([...exportJobs.filter(entry=>entry.id!==job.id),job]);
      if (job.preference_warning) storeDisplayPreference('export-name.'+editor,data.name);
      else {
        try { localStorage.removeItem('editroom.export-name.'+editor); } catch (_) {}
        if (projectId===editor) {
          project.export_name=data.name;
          savedProject.export_name=data.name;
        }
      }
      const warning=await rememberExportDestination(sheet,editor);
      if (projectId===editor) status(['Queued '+job.name,job.preference_warning,warning].filter(Boolean).join(' · '));
      refreshExportQueue();
    }
  } finally {
    submittingExport=false;
    updateExportAvailability();
  }
};

const exportQueue=byId('export-queue');
const pendingExportFiles=new Map();
let exportFolderWrites=Promise.resolve();
const exportJobNodes=new Map();
let exportJobs=[];
let exportQueueReading=false;
let exportQueuePoll=0;
let exportQueueVersion=0;

const exportActive=job=>['queued','running','cancelling'].includes(job.status);

function exportJobStatus(job) {
  if (job.status==='running') {
    const percent=Math.floor(job.progress*100);
    if (!percent) return 'Preparing';
    const left=job.eta_seconds;
    const eta=left>=60 ? Math.floor(left/60)+'m '+(left%60)+'s' : left+'s';
    return percent+'%'+(left!=null ? ' · '+eta+' left' : '');
  }
  if (job.status==='failed') return job.error || 'Export failed';
  if (job.status==='done' && job.error) return job.error;
  return {queued:'Waiting',cancelling:'Cancelling…',cancelled:'Cancelled',done:'Done'}[job.status];
}

function updateExportQueue(jobs) {
  exportJobs=jobs;
  exportQueue.hidden=!jobs.length;
  if (!jobs.length) exportQueue.open=false;
  const progress=byId('export-queue-progress');
  const running=jobs.find(job=>job.status==='cancelling' || job.status==='running');
  progress.hidden=running?.status!=='running';
  if (!progress.hidden) progress.value=Math.max(0,running.progress*100);
  const waiting=jobs.filter(job=>job.status==='queued').length;
  const pill=running ? exportJobStatus(running) : waiting ? 'Waiting'
    : jobs.some(job=>job.status==='failed') ? 'Export failed'
    : jobs.some(job=>job.status==='done') ? 'Exported' : 'Exports';
  byId('export-queue-status').textContent=pill+(waiting && running ? ' +'+waiting : '');

  const list=byId('export-jobs');
  const current=new Set(jobs.map(job=>job.id));
  for (const [id,node] of exportJobNodes) {
    if (!current.has(id)) { node.remove(); exportJobNodes.delete(id); }
  }
  for (const job of jobs) {
    let node=exportJobNodes.get(job.id);
    if (!node) {
      node=item('div','export-queue-job','');
      node.dataset.exportId=job.id;
      const title=item('strong','',job.name);
      title.title=job.name;
      node.append(title,item('small','',''));
      exportJobNodes.set(job.id,node);
      list.append(node);
    }
    const projectTitle=state?.projects.find(project=>project.id===job.project)?.title;
    node.querySelector('small').textContent=(projectTitle ? projectTitle+' · ' : '')+exportJobStatus(job);
    if (node.dataset.status===job.status) continue;
    node.dataset.status=job.status;
    node.querySelector('.button')?.remove();
    if (exportActive(job)) {
      const cancel=item('button','button','Cancel');
      cancel.type='button';
      cancel.disabled=job.status==='cancelling';
      cancel.setAttribute('aria-label','Cancel '+job.name);
      cancel.onclick=async()=>{
        cancel.disabled=true;
        try {
          const updated=await post('/api/exports/'+job.id+'/cancel',{});
          exportQueueVersion++;
          updateExportQueue(exportJobs.map(entry=>entry.id===job.id ? updated : entry));
          refreshExportQueue();
        } catch (error) {
          cancel.disabled=false;
          status(error.message);
        }
      };
      node.append(cancel);
    } else if (job.status==='done') {
      const download=exportDownload(job);
      download.textContent='Download';
      download.setAttribute('aria-label','Download '+job.name);
      node.append(download);
    }
  }
  byId('export-clear').hidden=!jobs.some(job=>!exportActive(job));
}

function exportDownload(job) {
  const link=item('a','button','');
  link.href='/api/exports/'+job.id;
  link.download=job.name;
  return link;
}

async function openExportStream(job) {
  const response=await fetch('/api/exports/'+job.id);
  if (!response.ok || !response.body) throw new Error('Could not open the exported file.');
  return response.body;
}
async function deliverToExportFolder(job,destination) {
  const {folder,filename,original}=destination;
  if (job.name!==filename) throw new Error('The export filename changed before delivery.');
  const deliver=async()=>{
    const stream=await openExportStream(job);
    try {
      const found=await findExportFile(folder,filename);
      if (!sameExportFile(original,found?.file ?? null)) {
        throw new Error('A file at that location changed while the export was running.');
      }
      const handle=found?.handle || await folder.getFileHandle(filename,{create:true});
      const initial=found?.file || await handle.getFile();
      if (!found && initial.size!==0) throw new Error('A file appeared at that location.');
      const writer=await handle.createWritable();
      try {
        await stream.pipeTo(writer,{preventClose:true});
        if (!sameExportFile(initial,await handle.getFile())) {
          throw new Error('A file at that location changed before the export finished saving.');
        }
        await writer.close();
      } catch (error) {
        await writer.abort().catch(()=>{});
        throw error;
      }
    } catch (error) {
      await stream.cancel().catch(()=>{});
      throw error;
    }
  };
  if (navigator.locks) await navigator.locks.request('editroom-export/'+folder.name+'/'+filename,deliver);
  else {
    exportFolderWrites=exportFolderWrites.then(deliver,deliver);
    await exportFolderWrites;
  }
}
async function deliverExport(job,pending) {
  try {
    if (pending.folder) {
      await deliverToExportFolder(job,pending);
    } else if (pending.saveTo) {
      const stream=await openExportStream(job);
      try { await stream.pipeTo(await pending.saveTo.createWritable()); }
      catch (error) { await stream.cancel().catch(()=>{});throw error; }
    } else {
      exportDownload(job).click();
    }
    if (projectId===pending.editor) {
      status('Exported '+(pending.saveTo?.name || job.name)+(pending.folder ? ' to '+pending.folder.name : ''));
    }
  } catch (error) {
    if (projectId===pending.editor) status('Download '+job.name+' from the export queue. '+error.message);
  }
}

async function refreshExportQueue() {
  if (exportQueueReading || byId('studio').hidden) return;
  exportQueueReading=true;
  const version=exportQueueVersion;
  try {
    const jobs=await api('/api/exports');
    if (byId('studio').hidden || version!==exportQueueVersion) return;
    updateExportQueue(jobs);
    for (const job of jobs) {
      if (exportActive(job)) continue;
      const pending=pendingExportFiles.get(job.id);
      if (!pending) continue;
      pendingExportFiles.delete(job.id);
      if (job.status==='done') deliverExport(job,pending);
    }
  } catch (_) {
    // Retry while the studio session stays open.
  } finally {
    exportQueueReading=false;
  }
}

byId('export-clear').onclick=async()=>{
  try {
    await post('/api/exports/clear',{});
    exportQueueVersion++;
    updateExportQueue(exportJobs.filter(exportActive));
    refreshExportQueue();
  } catch (error) { status(error.message); }
};
exportQueue.addEventListener('toggle',()=>{ if (exportQueue.open) refreshExportQueue(); });
exportQueue.addEventListener('keydown',event=>{
  if (event.key!=='Escape' || !exportQueue.open) return;
  event.preventDefault();event.stopPropagation();
  exportQueue.open=false;
  exportQueue.querySelector('summary').focus();
});
document.addEventListener('pointerdown',event=>{
  if (!exportQueue.contains(event.target)) exportQueue.open=false;
});
setInterval(()=>{
  if (document.hidden || byId('studio').hidden) return;
  if (exportJobs.some(exportActive) || exportQueue.open || ++exportQueuePoll%6===0) refreshExportQueue();
},600);

byId('login-form').onsubmit = async event => {
  event.preventDefault();
  const input = byId('owner-key');
  try {
    await post('/api/login', { key: input.value.trim() });
    input.value = '';
    byId('login-error').textContent = '';
    await refresh();
  } catch (error) { byId('login-error').textContent = error.message; }
};
const accountForm=byId('account-form');
const accountTestInbox=byId('account-test-inbox');
let accountMode=false;
let accountStep='signin';
let recoveryToken=null;
let billingPack=null;

const accountLabels={
  signin:['Sign in to your studio','Sign in'],
  signup:['Create an account','Sign up'],
  recover:['Reset your password','Send reset email'],
  'new-password':['Choose a new password','Update password'],
};

function showAccountStatus(message='',withInbox=false) {
  byId('account-status').textContent=message;
  accountTestInbox.hidden=!(withInbox && accountTestInbox.hasAttribute('href'));
}

function showAccountForm(step) {
  accountStep=step;
  const email=byId('account-email');
  const password=byId('account-password');
  byId('login-title').textContent=accountLabels[step][0];
  byId('account-submit').textContent=accountLabels[step][1];
  byId('account-mode').textContent=step==='signin' ? 'Create an account' : 'Back to sign in';
  byId('account-recover').hidden=step!=='signin';
  email.hidden=email.disabled=byId('account-email-label').hidden=step==='new-password';
  password.hidden=password.disabled=byId('account-password-label').hidden=step==='recover';
  password.autocomplete=step==='signin' ? 'current-password' : 'new-password';
  password.value='';
  byId('account-password-label').textContent=step==='new-password' ? 'New password' : 'Password';
  showAccountStatus();
  byId('login-error').textContent='';
  if (step!=='new-password') recoveryToken=null;
}

byId('account-mode').onclick=()=>showAccountForm(accountStep==='signin' ? 'signup' : 'signin');
byId('account-recover').onclick=()=>showAccountForm('recover');
accountForm.onsubmit=async event=>{
  event.preventDefault();
  const submit=byId('account-submit');
  if (!accountMode || submit.disabled) return;
  const email=byId('account-email');
  const password=byId('account-password');
  const step=accountStep;
  const fields=step==='recover' ? {email:email.value}
    : step==='new-password' ? {token:recoveryToken,password:password.value}
    : {email:email.value,password:password.value};
  password.value='';
  showAccountStatus();
  byId('login-error').textContent='';
  submit.disabled=byId('account-mode').disabled=byId('account-recover').disabled=true;
  try {
    const result=await post('/api/account/'+step,fields);
    if (step==='signin') {
      email.value='';
      await refresh();
    } else {
      showAccountForm('signin');
      const localInbox=accountTestInbox.hasAttribute('href');
      const confirmSignup=step==='signup' && result?.confirm_email;
      const messages={
        signup:localInbox && confirmSignup ? 'Open the confirmation email in the test inbox, then sign in.' : result?.message,
        recover:localInbox ? 'If an account exists for that email, its reset link will arrive in the test inbox.'
          : 'If an account exists for that email, a reset link is on its way.',
        'new-password':'Password updated. Sign in with your new password.',
      };
      showAccountStatus(messages[step],confirmSignup || step==='recover');
      (step==='new-password' ? email : password).focus();
    }
  } catch(error) {
    byId('login-error').textContent=error.message;
  } finally {
    submit.disabled=byId('account-mode').disabled=byId('account-recover').disabled=false;
  }
};
byId('signout').onclick = async () => {
  await post('/api/logout');
  showLogin();
};
const providerNames={open_ai:'ChatGPT',claude:'Claude',local:'Local',fixture:'Test planner'};
const effortLabel={none:'None',low:'Low',medium:'Medium',high:'High',xhigh:'Extra high',max:'Maximum'};
const providerDialog=byId('provider-dialog');
let providerSetup=null;

function providerProfile(name) {
  return state?.provider_connections?.find(connection=>connection.name===name);
}

function providerDescription(profile) {
  const model=state.provider_models?.[profile.name]?.options.find(model=>model.id===profile.model)?.label ?? profile.model;
  return [providerNames[profile.name],model,effortLabel[profile.effort]]
    .filter((label,index,labels)=>label && labels.indexOf(label)===index).join(' · ');
}

function updateProviderEffort() {
  const name=byId('provider').value;
  const model=byId('provider-model').value;
  const optionData=state.provider_models[name].options.find(row=>row.id===model);
  const efforts=optionData?.efforts || [];
  const field=byId('provider-effort');
  const saved=providerProfile(name);
  field.replaceChildren(option('Automatic',''),...efforts.map(effort=>option(effortLabel[effort],effort)));
  field.value=saved?.model===model && efforts.includes(saved.effort) ? saved.effort : '';
  field.hidden=byId('provider-effort-label').hidden=!efforts.length;
}

let providerCheck = null;

function providerCheckStatus(message='',result='') {
  const status=byId('provider-check-status');
  status.textContent=message;
  status.hidden=!message;
  status.dataset.result=result;
  status.classList.toggle('error',result==='failed');
}

function resetProviderCheck() {
  providerCheck?.abort();
  providerCheck=null;
  byId('provider-check').disabled=false;
  providerCheckStatus();
}

function providerFormData() {
  return {
    provider:byId('provider').value,
    key:byId('provider-key').value.trim() || null,
    model:byId('provider-model').value,
    effort:byId('provider-effort').value
  };
}

function updateProviderFields() {
  resetProviderCheck();
  const name=byId('provider').value;
  const saved=providerProfile(name);
  const catalogue=state.provider_models[name];
  const field=byId('provider-model');
  field.replaceChildren(...catalogue.options.map(model=>option(model.label,model.id)));
  field.value=saved?.model || catalogue.default;
  const key=byId('provider-key');
  key.value='';
  key.hidden=byId('provider-key-label').hidden=byId('provider-key-note').hidden=name==='local';
  byId('provider-check').hidden=name==='local';
  key.required=name!=='local' && !saved;
  key.placeholder=saved ? 'Leave blank to keep the connected key' : '';
  byId('provider-save').textContent=providerSetup?.draft
    ? (saved ? 'Save and send' : 'Connect and send')
    : (saved ? 'Save settings' : 'Connect');
  byId('disconnect-provider').hidden=!saved;
  byId('provider-error').textContent='';
  updateProviderEffort();
}

const rendererField=byId('renderer');
const rendererCheckButton=byId('renderer-check');
const rendererCheckStatus=byId('renderer-check-status');
let rendererCheck;
let savingRenderer=false;

function showRendererCheck(message='',result='') {
  rendererCheckStatus.textContent=message;
  rendererCheckStatus.hidden=!message;
  rendererCheckStatus.dataset.result=result;
  rendererCheckStatus.classList.toggle('error',result==='failed');
}

function resetRendererCheck() {
  rendererCheck?.abort();
  rendererCheck=null;
  rendererCheckButton.disabled=savingRenderer;
  showRendererCheck();
}

rendererField.onchange=async()=>{
  const editor=projectId;
  savingRenderer=true;
  rendererField.disabled=true;
  resetRendererCheck();
  try {
    await post(projectUrl('/renderer'),{model:rendererField.value});
    await refresh();
  } catch(error) {
    if (projectId===editor) rendererField.value=project.sequence.render_model;
    showRendererCheck(error.message,'failed');
  } finally {
    savingRenderer=false;
    rendererField.disabled=false;
    rendererCheckButton.disabled=false;
  }
};

rendererCheckButton.onclick=async()=>{
  const editor=projectId;
  const check=new AbortController();
  rendererCheck=check;
  rendererCheckButton.disabled=true;
  showRendererCheck('Checking the private GPU…');
  try {
    const report=await api(projectUrl('/renderer/check'),{signal:check.signal});
    if (rendererCheck!==check || projectId!==editor) return;
    if (report.model!==rendererField.value) {
      rendererField.value=report.model;
      showRendererCheck('The video model changed. Check the GPU again.');
      return;
    }
    showRendererCheck(report.message,report.ready?'passed':'failed');
  } catch(error) {
    if (rendererCheck===check && projectId===editor) showRendererCheck(error.message,'failed');
  } finally {
    if (rendererCheck===check) {
      rendererCheck=null;
      rendererCheckButton.disabled=savingRenderer;
    }
  }
};

function openProviderSettings(name,sendDraft=false) {
  if (providerDialog.open) return;
  providerSetup={draft:sendDraft ? {project:projectId,text:byId('prompt').value} : null};
  const field=byId('provider');
  if (state.provider_models[name]) field.value=name;
  rendererField.value=project.sequence.render_model;
  resetRendererCheck();
  updateProviderFields();
  providerDialog.showModal();
}

byId('provider-button').onclick=()=>openProviderSettings(state.provider);
byId('provider').onchange=updateProviderFields;
byId('provider-model').onchange=()=>{
  resetProviderCheck();
  updateProviderEffort();
};
byId('provider-key').oninput=resetProviderCheck;
byId('provider-check').onclick=async()=>{
  const input=byId('provider-key');
  if (!input.reportValidity()) return;
  resetProviderCheck();
  const check=new AbortController();
  providerCheck=check;
  byId('provider-check').disabled=true;
  byId('provider-error').textContent='';
  providerCheckStatus('Checking model access…');
  try {
    const report=await api('/api/provider/check',{method:'POST',data:providerFormData(),signal:check.signal});
    if (providerCheck===check) providerCheckStatus(report.message,report.accessible?'passed':'failed');
  } catch (error) {
    if (providerCheck===check) providerCheckStatus(error.message,'failed');
  } finally {
    if (providerCheck===check) {
      providerCheck=null;
      byId('provider-check').disabled=false;
    }
  }
};
byId('provider-form').onsubmit = async event => {
  event.preventDefault();
  const save=byId('provider-save');
  if (save.disabled || !providerSetup) return;
  const setup=providerSetup;
  const request=providerFormData();
  byId('provider-key').value='';
  resetProviderCheck();
  byId('provider-check').disabled=true;
  save.disabled=true;
  try {
    await post('/api/provider',request);
    await refresh();
    if (!providerDialog.open || providerSetup!==setup) return;
    const {draft}=setup;
    const send=draft && projectId===draft.project &&
      byId('prompt').value===draft.text && state.provider===request.provider;
    providerDialog.close();
    if (send) byId('prompt-form').requestSubmit();
  } catch (error) {
    if (providerSetup===setup) byId('provider-error').textContent=error.message;
  } finally {
    save.disabled=false;
    byId('provider-check').disabled=false;
  }
};
byId('disconnect-provider').onclick=async()=>{
  try {
    await post('/api/provider/disconnect',{provider:byId('provider').value});
    await refresh();
    updateProviderFields();
  } catch (error) { byId('provider-error').textContent=error.message; }
};
providerDialog.addEventListener('close', () => {
  if (providerDialog.open) return;
  providerSetup=null;
  byId('provider-key').value='';
  resetProviderCheck();
  resetRendererCheck();
});
for (const close of document.querySelectorAll('dialog .close')) close.onclick = () => close.closest('dialog').close();
byId('credit-button').onclick = () => byId('credits-dialog').showModal();
byId('buy-credits').onclick=async()=>{
  const button=byId('buy-credits');
  button.disabled=true;
  byId('credit-error').textContent='';
  byId('checkout-status').textContent='Opening Stripe test checkout…';
  try {
    const checkout=await post('/api/account/checkout');
    location.assign(checkout.url);
  } catch(error) {
    byId('checkout-status').textContent='';
    byId('credit-error').textContent=error.message;
    button.disabled=false;
  }
};
byId('credit-form').onsubmit = async event => {
  event.preventDefault();
  try {
    await post('/api/credits', { amount: Number(byId('credit-amount').value) });
    byId('credit-error').textContent = '';
    await refresh();
  } catch (error) { byId('credit-error').textContent = error.message; }
};
const promptDraftKey=editor=>'editroom.prompt-draft.'+editor;

function savePromptDraft(value,editor=projectId) {
  if (!editor) return;
  const key=promptDraftKey(editor);
  const text=value.trim();
  if (text && text!=='@selected') sessionStorage.setItem(key,value);
  else sessionStorage.removeItem(key);
}

function restorePromptDraft() {
  const handoff=sessionStorage.getItem('editroom.draft');
  if (handoff!==null) {
    if (handoff.trim()) {
      savePromptDraft(handoff);
      directorEnabled=true;
      directorPanelOpen=true;
      storeDisplayPreference('director-enabled','true');
      storeDisplayPreference('director-open','true');
      updateDirectorPanel();
    }
    sessionStorage.removeItem('editroom.draft');
  }
  closeMentions();
  byId('prompt').value=sessionStorage.getItem(promptDraftKey(projectId)) || '';
  byId('director-error').textContent='';
}

function directorBusy() {
  return Boolean(presentedFrame?.external || state?.runs.some(run=>run.status==='running'));
}

byId('prompt-form').onsubmit = async event => {
  event.preventDefault();
  const input = byId('prompt');
  const draft = input.value;
  const prompt = draft.trim();
  if (!prompt || submittingDirector || byId('send').disabled) return;
  if (!state.provider) return openProviderSettings(state.provider,true);
  const editor=projectId;
  const duration = byId('duration').value;
  submittingDirector=true;
  byId('send').disabled = true;
  savePromptDraft(draft,editor);
  try {
    const run = await post(projectUrl('/director'), { prompt, selected, selected_clips:[...selection],
      selected_media:pickedMedia(), duration:duration ? Number(duration) : null, quick, playhead:frameAt });
    if (sessionStorage.getItem(promptDraftKey(editor))===draft) savePromptDraft('',editor);
    if (projectId===editor) {
      shownRun = run.id;
      if (input.value===draft) {
        input.value='';
        closeMentions();
      }
      byId('director-error').textContent='';
    }
    await refresh();
  } catch (error) {
    if (projectId===editor) byId('director-error').textContent=error.message;
  } finally {
    submittingDirector=false;
    byId('send').disabled=directorBusy();
  }
};
async function initialise() {
  // The optional desktop launcher uses a fragment, which is never sent in the request URL.
  const fragment = new URLSearchParams(location.hash.slice(1));
  let accountConfig;
  try {
    accountConfig=await api('/api/account/config');
    if (typeof accountConfig?.enabled!=='boolean') throw new Error('Invalid sign-in configuration.');
  } catch {
    byId('login-error').textContent='Could not load sign-in. Refresh the page to try again.';
    showLogin();
    return;
  }
  accountMode=accountConfig.enabled;
  billingPack=accountConfig.billing;
  byId('login-form').hidden=byId('local-login-note').hidden=accountMode;
  if (accountMode) {
    if (accountConfig.test_inbox_url) accountTestInbox.href=accountConfig.test_inbox_url;
    accountForm.hidden=false;
    showAccountForm('signin');
    if (fragment.has('access_token') || fragment.has('refresh_token')) {
      history.replaceState(null, '', location.pathname);
      if (fragment.get('type')==='recovery' && fragment.get('access_token')) {
        recoveryToken=fragment.get('access_token');
        showAccountForm('new-password');
        showLogin();
        byId('account-password').focus();
        return;
      }
      showAccountStatus('Sign in to open the studio.');
    }
  }
  if (fragment.has('sign-in')) {
    history.replaceState(null, '', location.pathname);
    try { if (!accountMode) await post('/api/login', { key: fragment.get('sign-in') }); }
    catch (error) { byId('login-error').textContent = error.message; }
  }
  await refresh();
  const checkout=new URLSearchParams(location.search).get('checkout');
  if (billingPack && (checkout==='returned' || checkout==='cancelled')) {
    history.replaceState(null,'',location.pathname);
    byId('checkout-status').textContent=checkout==='returned'
      ? 'Credits update when Stripe confirms payment.'
      : 'Checkout cancelled.';
    byId('credits-dialog').showModal();
  }
}
initialise().catch(() => showLogin());
let polling = false;
setInterval(async () => {
  if (polling || byId('studio').hidden || document.hidden) return;
  polling = true;
  try { await refresh(); } catch (_) { /* Retry on the next update. */ }
  finally { polling = false; }
}, 2000);

function placePlayhead(at) {
  frameAt = at;
  if (playingCut) {
    const x=at*pxPerTick;
    const visible=x-timelineBody.scrollLeft;
    if (visible>timelineViewWidth()-28 || visible<0) timelineBody.scrollLeft=x-48;
  }
  const field = byId('timecode');
  if (document.activeElement !== field) field.value = tc(displayedPlayhead());
  drawTimelinePlayhead();
  drawComposed();
}
function displayedPlayhead() {
  return !playingCut && !waitingForAudio ? retimeProjection?.focus ?? frameAt : frameAt;
}
function drawTimelinePlayhead() {
  const left = (Math.max(0,displayedPlayhead()) * pxPerTick) + 'px';
  byId('playhead-line').style.left = left;
  byId('ruler-playhead').style.left = left;
}
function previewSource(source, at = 0) {
  if (source.missing_file) return status('Locate '+source.missing_file+' in Media.');
  clearLibrarySkim();
  const wasPlaying=playingCut || waitingForAudio;
  pauseTimeline();
  if (wasPlaying) sendPlayback('pause');
  sourcePreview = source;
  compositeCanvas.hidden = true;
  preview.pause();
  byId('audio-preview-name').textContent = source.video ? '' : source.title;
  byId('audio-preview-name').hidden = source.video;
  byId('empty-preview').hidden = true;
  preview.hidden = !source.video;
  preview.poster = source.video ? sourceUrl('thumb',source.id) : '';
  preview.onloadedmetadata = () => {
    if (at > 0 && at < preview.duration) preview.currentTime = at;
  };
  preview.src = mediaUrl(source.id);
  byId('preview-title').textContent = source.title;
  byId('timecode').value = tc(ticks(at));
  byId('play-cut').disabled = source.image;
}
function seekTimeline(at, scrub = false, notify = !scrub) {
  clearLibrarySkim();
  leaveSourcePreview();
  if (playingCut || waitingForAudio) pauseTimeline();
  clearSkim(false);
  at = Math.max(0,Math.min(ticks(7200),onFrame(at)));
  placePlayhead(at);
  if (!scrub) {
    const room=timelineViewWidth();
    const x=at*pxPerTick;
    if (x+14>byId('ruler').clientWidth) drawRuler();
    if (x<timelineBody.scrollLeft || x>timelineBody.scrollLeft+room-10) timelineBody.scrollLeft=Math.max(0,x-room*.5);
  }
  if (scrub) scrubAudio(at);
  if (notify) sendPlayback('seek');
}
function clearSkim(restore = true) {
  if (skimFrame==null) return;
  skimFrame=null;
  byId('skimmer-line').hidden=true;
  if (restore) drawComposed();
}
function skimTimeline(at) {
  if (!toggled('skim') || playingCut || waitingForAudio || !project) return;
  leaveSourcePreview();
  const point=Math.min(clipTotal(),Math.max(0,onFrame(at)));
  if (skimFrame===point) return;
  skimFrame=point;
  const line=byId('skimmer-line');
  line.style.left=(point*pxPerTick)+'px';
  line.hidden=false;
  drawComposed();
  scrubAudio(point);
}
function rulerValue(event) {
  return Math.max(0,(event.clientX-byId('ruler').getBoundingClientRect().left)/pxPerTick);
}
function rulerSnap(at,options) {
  return Math.max(0,Math.min(ticks(7200),onFrame(toggled('snap')
    ? snapOffset(at,[0],new Set(),{pixels:6,...options}).delta : at)));
}
function beginRulerScrub(event) {
  if (event.button!==0) return;
  event.preventDefault();
  event.stopPropagation();
  byId('timeline-body').focus({preventScroll:true});
  const scrub=pointer=>seekTimeline(rulerSnap(rulerValue(pointer),{includePlayhead:false}),true);
  scrub(event);
  timelineGesture(event,{update:scrub,complete:()=>sendPlayback('seek'),autoscroll:"x"});
}
function normalizedMarkers() {
  return [...project.sequence.markers].sort((a,b)=>a.frame-b.frame);
}

const markColors={white:'#ededea',yellow:'#f2b33d',green:'#6cc08b',blue:'#5b9bf0',purple:'#b07cf0',red:'#f0503c'};
function rulerExtent() {
  const seq=project.sequence;
  const duration=retimeProjection?.duration ?? clipTotal();
  const last=Math.max(duration,frameAt,displayedPlayhead(),seq.in_point??0,seq.out_point??0,...seq.markers.map(m=>m.frame));
  return Math.max(ticks(30),duration*1.3+ticks(10),last+ticks(2));
}
function timelineViewWidth() {
  return Math.max(0,timelineBody.clientWidth-timelineBody.querySelector('.ruler-label').offsetWidth);
}
function minimumTimelineZoom() {
  return Math.min(maxTimelineScale,Math.max(.05/TICKS_PER_SECOND,timelineViewWidth()/rulerExtent()));
}
function drawRulerWidth() {
  const contentWidth=Math.max(timelineViewWidth(),rulerExtent()*pxPerTick);
  timelineBody.querySelector('.timeline-strip').style.width=(contentWidth+timelineBody.querySelector('.ruler-label').offsetWidth)+'px';
}
function drawRuler() {
  drawRulerWidth();
  drawRulerTicks();
  drawRulerMarks();
  drawMarkOverlays();
  drawMarkerPanel();
  drawTimelinePlayhead();
  byId('skimmer-line').style.left=((skimFrame??0)*pxPerTick)+'px';
  drawTimelineBars();
  scheduleTimelineMedia();
}
function drawRulerTicks() {
  if (!project) return;
  const body=byId('timeline-body');
  const pxPerSecond=pxPerTick*TICKS_PER_SECOND;
  const frame=frameStep()/TICKS_PER_SECOND;
  const steps=[[frame,1],[2*frame,2],[5*frame,5],[10*frame,5],[1,4],[2,4],[5,5],[10,5],[15,3],[30,6],
    [60,6],[120,4],[300,5],[600,5],[900,3],[1800,6],[3600,6],[7200,4]];
  const [major,parts]=steps.find(([span])=>span*pxPerSecond>=86&&(span>=1||span<.5))||steps.at(-1);
  const start=Math.floor(body.scrollLeft/(major*pxPerSecond));
  const stop=Math.ceil((body.scrollLeft+body.clientWidth)/(major*pxPerSecond));
  const fragment=document.createDocumentFragment();
  for (let i=start;i<=stop;i++) {
    for(let j=0;j<parts;j++) {
      const secs=(i+j/parts)*major;
      const node=item('i',j?'ruler-tick minor':'ruler-tick','');
      if (!j) {
        const at=ticks(secs);
        node.textContent=major>=1 ? clockTime(at,false) : timeStyle==='simple' ? clockTime(at,true)
          : clockTime(ticks(Math.floor(secs+1e-6)),false)+':'+tc(at).split(':').at(-1);
      }
      node.style.left=(secs*pxPerSecond)+'px';
      fragment.append(node);
    }
  }
  byId('ruler-ticks').replaceChildren(fragment);
}
function markerFrame(mark) {
  return markerDraft?.id===mark.id && markerDraft.frame!=null ? markerDraft.frame : mark.frame;
}
function markerName(mark) {
  return markerDraft?.id===mark.id && markerDraft.name!=null ? markerDraft.name : mark.name;
}
function markerColor(mark) { return markColors[mark.color] || markColors.yellow; }
function markerButton(id) {
  return [...byId('ruler-marks').querySelectorAll('[data-marker]')].find(el=>el.dataset.marker===id);
}
function drawRulerMarks() {
  const overlay=byId('ruler-marks');
  if (overlay.contains(document.activeElement)) byId('timeline-body').focus({preventScroll:true});
  overlay.replaceChildren(...normalizedMarkers().map(mark=>{
    const button=item('button','marker','');
    button.type='button';
    button.dataset.marker=mark.id;
    button.dataset.frame=mark.frame;
    button.style.setProperty('--marker-color',markerColor(mark));
    button.style.left=(markerFrame(mark)*pxPerTick)+'px';
    button.textContent=markerName(mark);
    button.title=markerName(mark)+' · '+tc(markerFrame(mark));
    button.setAttribute('aria-label','Edit marker '+(markerName(mark)||tc(mark.frame)));
    // Pointer release is handled by the gesture. Keyboard activation opens the same inspector.
    button.onclick=e=>{if (e.detail===0) openMarkerPanel(mark.id);};
    button.onpointerdown=e=>dragMarker(e,mark);
    button.oncontextmenu=e=>openTimelineMenu(e,[
      ['Edit Marker',()=>openMarkerPanel(mark.id)],['Go to Marker',()=>seekTimeline(mark.frame)],
      ['Delete Marker',()=>writeMarker('delete_markers',{marker_ids:[mark.id]})]
    ],'Marker actions');
    return button;
  }));
  for (const [edge,point] of [['in',project.sequence.in_point],['out',project.sequence.out_point]]) {
    if (point==null) continue;
    const bracket=item('button','range-bracket '+edge,'');
    bracket.type='button';
    bracket.dataset.edge=edge;
    bracket.dataset.frame=point;
    bracket.style.left=(point*pxPerTick)+'px';
    bracket.title=(edge==='in'?'In point':'Out point')+' · '+tc(point);
    bracket.setAttribute('aria-label',bracket.title);
    bracket.onpointerdown=e=>dragRangeEdge(e,edge,point);
    bracket.onkeydown=e=>{
      if (!['ArrowLeft','ArrowRight'].includes(e.key)) return;
      e.preventDefault();
      e.stopPropagation();
      const delta=(e.key==='ArrowRight'?1:-1)*frameStep()*(e.shiftKey?5:1);
      saveRange({[edge]:Math.max(0,point+delta)/TICKS_PER_SECOND,move_edge:true});
    };
    overlay.append(bracket);
  }
}
function drawMarkOverlays() {
  const body=byId('timeline-marks');
  const range=markedRange();
  const items=[];
  if (range) {
    const [first,last]=range;
    const before=item('i','range-shade before','');
    before.style.width=(first*pxPerTick)+'px';
    const after=item('i','range-shade after','');
    after.style.left=(last*pxPerTick)+'px';
    const bar=byId('ruler');
    bar.style.setProperty('--mark-start',(first*pxPerTick)+'px');
    bar.style.setProperty('--mark-end',(last*pxPerTick)+'px');
    bar.classList.add('has-marked-range');
    items.push(before,after);
  } else byId('ruler').classList.remove('has-marked-range');
  for (const mark of normalizedMarkers()) {
    const line=item('i','marker-line','');
    line.dataset.markerLine=mark.id;
    line.style.left=(markerFrame(mark)*pxPerTick)+'px';
    line.style.backgroundColor=markerColor(mark);
    items.push(line);
  }
  body.replaceChildren(...items);
  drawRangeReadout(range);
}
function drawRangeReadout(range) {
  const {in_point,out_point} = rangeDraft || project.sequence;
  byId('range-readout').hidden = in_point==null && out_point==null;
  byId('jump-in').querySelector('time').textContent = tc(in_point ?? 0);
  byId('jump-out').querySelector('time').textContent = tc(out_point ?? clipTotal());
  const seconds = range ? (range[1] - range[0]) / TICKS_PER_SECOND : 0;
  let label = '';
  if (seconds>=60) {
    const total=Math.round(seconds);
    const hours=Math.floor(total/3600), minutes=Math.floor(total/60)%60, remainder=String(total%60).padStart(2,'0');
    label=hours ? hours+':'+String(minutes).padStart(2,'0')+':'+remainder : minutes+':'+remainder;
  } else if (range) label = seconds.toFixed(seconds<10 ? 1 : 0)+'s';
  byId('range-length').textContent = label ? '· '+label : '';
}
for (const [id,point] of [['jump-in',()=>project.sequence.in_point ?? 0],['jump-out',()=>project.sequence.out_point ?? clipTotal()]]) {
  byId(id).onclick=()=>{
    seekTimeline(point());
    timelineBody.focus({preventScroll:true});
  };
}
byId('clear-range').onclick = () => {
  saveRange({clear:true});
  timelineBody.focus({preventScroll:true});
};
function onPlayhead(event) {
  return Math.abs((rulerValue(event)-frameAt)*pxPerTick)<=6;
}
function dragMarker(event,mark) {
  if (event.button!==0) return;
  if (onPlayhead(event)) {beginRulerScrub(event);return;}
  event.preventDefault();event.stopPropagation();
  byId('timeline-body').focus({preventScroll:true});
  const button=event.currentTarget;
  let overlay;
  const at=pointer=>rulerSnap(rulerValue(pointer),{ignore:mark.frame});
  timelineGesture(event,{
    update(pointer) {
      const frame=at(pointer);
      markerDraft={id:mark.id,frame,name:markerDraft?.id===mark.id?markerDraft.name:undefined};
      button.style.left=(frame*pxPerTick)+'px';
      drawMarkOverlays();
      overlay ||= item('span','ruler-readout','');
      if (!overlay.isConnected) document.body.append(overlay);
      overlay.textContent=tc(frame);
      overlay.style.left=(pointer.clientX+9)+'px';
      overlay.style.top=(pointer.clientY-18)+'px';
      drawMarkerPanel();
    },
    complete(pointer,moved) {
      if (moved) writeMarker('set_marker',{marker_id:mark.id,time:at(pointer)/TICKS_PER_SECOND});
      else openMarkerPanel(mark.id);
    },
    cancel() {
      overlay?.remove();
      if (markerDraft?.id===mark.id) delete markerDraft.frame;
      button.style.left=(mark.frame*pxPerTick)+'px';
      drawMarkOverlays();
      drawMarkerPanel();
    }
  });
}
function saveRange(args) {return writeMarker('set_in_out',args);}
function dragRangeEdge(event,edge,point) {
  if (event.button!==0) return;
  if (onPlayhead(event)) {beginRulerScrub(event);return;}
  event.preventDefault();event.stopPropagation();
  byId('timeline-body').focus({preventScroll:true});
  const button=event.currentTarget;
  const key=edge+'_point';
  let overlay;
  timelineGesture(event,{
    update(pointer) {
      const now=rulerSnap(rulerValue(pointer),{});
      rangeDraft={in_point:project.sequence.in_point,out_point:project.sequence.out_point,[key]:now};
      button.style.left=(now*pxPerTick)+'px';
      drawMarkOverlays();
      overlay ||= item('span','ruler-readout','');
      if (!overlay.isConnected) document.body.append(overlay);
      overlay.textContent=tc(now);
      overlay.style.left=(pointer.clientX+9)+'px';
      overlay.style.top=(pointer.clientY-18)+'px';
    },
    complete(pointer,moved) {
      if (moved) saveRange({[edge]:rulerSnap(rulerValue(pointer),{})/TICKS_PER_SECOND,move_edge:true});
    },
    cancel() {
      overlay?.remove();
      rangeDraft=null;
      button.style.left=(point*pxPerTick)+'px';
      drawMarkOverlays();
    }
  });
}
function writeMarker(tool,args,editor=projectId) {
  const call=()=>mcp(tool,args,{editor});
  markerWrites=markerWrites.then(call,call);
  return markerWrites;
}
function addMarkerAtPlayhead() {
  const frame=onFrame(playheadTicks());
  const existing=normalizedMarkers().find(mark=>Math.abs(mark.frame-frame)<frameStep()/2);
  if (existing) {
    openMarkerPanel(existing.id);
    return;
  }
  return writeMarker('set_marker',{time:frame/TICKS_PER_SECOND});
}
function markAtPlayhead(edge,at=playheadTicks()) {
  return saveRange({[edge]:at/TICKS_PER_SECOND});
}
function openMarksMenu(event) {
  const seq=project.sequence;
  openEditorCommands(event,'Mark',[
    ['Add Marker','addMarker'],
    ['Clear All Markers',()=>writeMarker('delete_markers',{all:true}),!seq.markers.length],
    ['Set In','setIn'],
    ['Set Out','setOut'],
    ['Clear In and Out','clearInOut',seq.in_point==null && seq.out_point==null],
  ]);
}
byId('mark-commands').onclick=byId('marks-menu').onclick=openMarksMenu;
byId('ruler').oncontextmenu=event=>{
  if (event.target.closest('[data-marker]')) return;
  const frame=rulerSnap(rulerValue(event),{includePlayhead:false});
  openTimelineMenu(event,[
    ['Add Marker Here',()=>{
      const existing=normalizedMarkers().find(mark=>Math.abs(mark.frame-frame)<frameStep()/2);
      return existing ? openMarkerPanel(existing.id) : writeMarker('set_marker',{time:frame/TICKS_PER_SECOND});
    }],
    ['Set In Here',()=>markAtPlayhead('in',frame)],
    ['Set Out Here',()=>markAtPlayhead('out',frame)],
    ['Clear In and Out',()=>saveRange({clear:true}),project.sequence.in_point==null && project.sequence.out_point==null],
  ],'Timeline marks');
};
function openMarkerPanel(id) {
  const mark=project.sequence.markers.find(mark=>mark.id===id);
  if (!mark) return;
  if (markerPanel?.id!==id) {
    flushMarkerRename();
    markerPanel={id,editor:projectId,pending:null};
    byId('marker-name').value=markerName(mark);
  }
  drawMarkerPanel();
  byId('marker-name').focus({preventScroll:true});
}
function drawMarkerPanel() {
  const pane=byId('marker-popover');
  if (!markerPanel || markerPanel.editor!==projectId) return;
  const mark=project.sequence.markers.find(mark=>mark.id===markerPanel.id);
  const anchor=markerButton(markerPanel.id);
  if (!mark || !anchor) {closeMarkerPanel(false);return;}
  pane.hidden=false;
  const field=byId('marker-name');
  if (document.activeElement!==field) field.value=markerName(mark);
  byId('marker-time').textContent=tc(markerFrame(mark));
  for (const button of byId('marker-colors').children) {
    button.setAttribute('aria-pressed',String(button.dataset.color===mark.color));
  }
  const list=byId('marker-list');
  const marks=normalizedMarkers();
  list.hidden=marks.length<2;
  const key=JSON.stringify(marks.map(row=>[row.id,markerFrame(row),markerName(row),row.color,row.id===mark.id]));
  if (list.dataset.rows!==key) {
    list.dataset.rows=key;
    list.replaceChildren(...marks.map(row=>{
      const button=item('button','marker-list-row','');
      button.type='button';
      button.setAttribute('aria-current',String(row.id===mark.id));
      const swatch=item('i','marker-list-tint','');
      swatch.style.backgroundColor=markerColor(row);
      button.append(swatch,item('span','marker-list-name',markerName(row)),item('time','',tc(markerFrame(row))));
      button.onclick=()=>{
        seekTimeline(markerFrame(row));
        openMarkerPanel(row.id);
      };
      return button;
    }));
  }
  const box=anchor.getBoundingClientRect();
  pane.style.left=Math.max(8,Math.min(innerWidth-pane.offsetWidth-8,box.left-18))+'px';
  const top=box.top-pane.offsetHeight-7;
  pane.style.top=Math.max(8,Math.min(innerHeight-pane.offsetHeight-8,top>=8?top:box.bottom+7))+'px';
}
function flushMarkerRename() {
  clearTimeout(markerRenameTimer);
  const panel=markerPanel;
  if (!panel || panel.pending==null) return;
  const name=panel.pending;
  panel.pending=null;
  writeMarker('set_marker',{marker_id:panel.id,name},panel.editor).then(()=>{
    if (projectId===panel.editor && markerDraft?.id===panel.id && markerDraft.name===name) {
      delete markerDraft.name;
      const mark=project.sequence.markers.find(mark=>mark.id===panel.id);
      const button=markerButton(panel.id);
      if (button && mark) button.textContent=mark.name;
      drawMarkerPanel();
    }
  });
}
function closeMarkerPanel(commit=true,focus=false) {
  if (commit) flushMarkerRename();
  else clearTimeout(markerRenameTimer);
  markerPanel=null;
  byId('marker-popover').hidden=true;
  if (focus) byId('timeline-body').focus({preventScroll:true});
}
byId('marker-name').oninput=event=>{
  if (!markerPanel) return;
  const id=markerPanel.id;
  const name=event.target.value;
  markerPanel.pending=name;
  markerDraft={id,name};
  const button=markerButton(id);
  if (button) {
    button.textContent=name;
    button.setAttribute('aria-label','Edit marker '+(name||tc(Number(button.dataset.frame))));
  }
  drawMarkerPanel();
  clearTimeout(markerRenameTimer);
  markerRenameTimer=setTimeout(flushMarkerRename,360);
};
byId('marker-form').onsubmit=event=>{
  event.preventDefault();
  closeMarkerPanel(true,true);
};
byId('marker-close').onclick=()=>closeMarkerPanel(true,true);
byId('marker-delete').onclick=()=>{
  if (!markerPanel) return;
  const {id,editor}=markerPanel;
  closeMarkerPanel(false,true);
  writeMarker('delete_markers',{marker_ids:[id]},editor);
};
for (const [color,value] of Object.entries(markColors)) {
  const button=item('button','marker-color','');
  button.type='button';
  button.dataset.color=color;
  button.style.setProperty('--swatch',value);
  const name=color[0].toUpperCase()+color.slice(1);
  button.setAttribute('aria-label',name);
  button.title=name;
  button.onclick=()=>{
    if (!markerPanel) return;
    flushMarkerRename();
    writeMarker('set_marker',{marker_id:markerPanel.id,color},markerPanel.editor);
  };
  byId('marker-colors').append(button);
}
document.addEventListener('pointerdown',event=>{
  if (markerPanel && !event.target.closest('#marker-popover,[data-marker]')) closeMarkerPanel();
});
byId('marker-popover').addEventListener('keydown',event=>{
  if (event.key==='Escape') {
    event.preventDefault();
    closeMarkerPanel(true,true);
  }
});

function toggled(id) { return byId(id).getAttribute('aria-pressed') === 'true'; }
function toggle(id) {
  byId(id).setAttribute('aria-pressed', String(!toggled(id)));
  return toggled(id);
}
for (const id of ['filmstrip','snap','sticky','skim','audio-scrub']) {
  const button=byId(id);
  const persistent=id!=='snap';
  if (persistent) button.setAttribute('aria-pressed',displayPreference('timeline-'+id,String(id==='filmstrip' || id==='skim')));
  button.onclick = event => {
    toggle(id);
    if (persistent) storeDisplayPreference('timeline-'+id,String(toggled(id)));
    if (id === 'filmstrip') {
      if (!toggled(id)) filmstripFrames.reset();
      renderProject();
    }
    if (id === 'audio-scrub' && toggled(id)) startAudioContext();
    if (id === 'skim' && !toggled(id)) clearSkim();
    if (event.detail) byId('timeline-body').focus({preventScroll:true});
  };
}
byId('select-tool').onclick = () => {
  editorTool = 'select';
  byId('select-tool').setAttribute('aria-pressed','true');
  byId('cut-tool').setAttribute('aria-pressed','false');
};
byId('cut-tool').onclick = () => {
  editorTool = 'cut';
  byId('cut-tool').setAttribute('aria-pressed','true');
  byId('select-tool').setAttribute('aria-pressed','false');
};
byId('loop').onclick = () => { playLoop = toggle('loop'); sendPlayback('set_loop'); };
function setMediaLayout(listMode) {
  clearLibrarySkim();
  byId('media-list').dataset.list = String(listMode);
  byId('media-layout').setAttribute('aria-pressed',String(!listMode));
}
function changeMediaLayout(listMode) {
  setMediaLayout(listMode);
  storeDisplayPreference('media-layout',listMode ? 'list' : 'thumbnails');
}
setMediaLayout(displayPreference('media-layout','thumbnails') === 'list');
byId('media-layout').onclick = () => {
  changeMediaLayout(byId('media-list').dataset.list !== 'true');
};
byId('media-search').oninput = () => {
  librarySearch = null;
  if (project) renderLibrary();
  queueLibrarySearch(170);
};
function zoomTimeline(factor) {
  if (!project) return;
  const width=timelineViewWidth();
  const currentX=frameAt*pxPerTick-timelineBody.scrollLeft;
  const anchor=currentX>=0 && currentX<=width ? currentX : width/2;
  const newScale=factor==null ? (width-80)/Math.max(TICKS_PER_SECOND,clipTotal()) : factor*pxPerTick;
  const scale=TimelineView.clamp(newScale,minimumTimelineZoom(),maxTimelineScale);
  setTimeView(scale,factor==null ? 0 : frameAt*scale-anchor);
}
function setTimeView(scale,scroll) {
  const next=TimelineView.clamp(scale,minimumTimelineZoom(),maxTimelineScale);
  if (Math.abs(next-pxPerTick)>=1e-9/TICKS_PER_SECOND) {
    pxPerTick=next;
    renderTimelineTracks();
  }
  timelineBody.scrollLeft=scroll;
  drawRulerTicks();
  drawMarkerPanel();
  drawTimelineBars();
}
for (const [id,factor] of [['zoom-timeline-out',1/1.4],['zoom-timeline-in',1.4],['fit-timeline',null]]) {
  byId(id).onclick=event=>{
    zoomTimeline(factor);
    if (event.detail) timelineBody.focus({preventScroll:true});
  };
}
timelineBody.addEventListener('wheel',event=>{
  const units = event.deltaMode===1 ? 12 : event.deltaMode===2 ? timelineBody.clientHeight : 1;
  if (event.altKey || event.metaKey || event.ctrlKey) {
    event.preventDefault();
    zoomTimeline(Math.exp(-event.deltaY*(event.deltaMode===1 ? 8 : units)*.012));
    return;
  }
  let {deltaX:x,deltaY:y} = event;
  if ((!x && event.shiftKey) || (!x && event.deltaMode===1)) [x,y]=[y,0];
  if (!x && !y) return;
  event.preventDefault();
  if (Math.abs(x)>=Math.abs(y)) timelineBody.scrollLeft+=x*units;
  else timelineBody.scrollTop+=y*units;
},{passive:false});

const timeBar = byId('timeline-time-bar');
const rowsBar = byId('timeline-rows-bar');
function barGeometry(vertical) {
  const bar = vertical ? rowsBar.clientHeight : timeBar.clientWidth;
  const extent = vertical ? timelineRows.world : rulerExtent();
  const visible = vertical ? timelineRows.visible : timelineViewWidth();
  const world = vertical ? extent : Math.max(visible,extent*pxPerTick);
  const scroll = vertical ? timelineBody.scrollTop : timelineBody.scrollLeft;
  return {bar,extent,visible,world,scroll,...TimelineView.thumb(bar,world,visible,scroll)};
}
function drawTimelineBars() {
  if (!project || !timelineRows) return;
  for (const [element,vertical] of [[timeBar,false],[rowsBar,true]]) {
    const {visible,world,scroll,drawn,offset} = barGeometry(vertical);
    const thumb=element.firstElementChild;
    thumb.style[vertical?'height':'width']=drawn+'px';
    thumb.style[vertical?'top':'left']=offset+'px';
    const position=Math.round(100*TimelineView.clamp(scroll/Math.max(1,world-visible),0,1));
    element.setAttribute('aria-valuenow',String(position));
    const scale=vertical ? timelineRows.height+' px track height' : (pxPerTick*TICKS_PER_SECOND).toFixed(1)+' px per second';
    element.setAttribute('aria-valuetext',position+'% scrolled, '+scale);
  }
}
function resizeRows(height) {
  const old=timelineRows;
  const target=TimelineView.clamp(height,TimelineView.minRow,TimelineView.maxRow);
  const next=TimelineView.rows(target,old.visible,old.video,old.audio,old.captions);
  const anchor=TimelineView.rowAnchor(timelineBody.scrollTop-old.top+old.visible/2,old);
  setRowView(next,anchor.fixed+anchor.rows*target-next.visible/2);
  drawTimelineBars();
}
for (const [element,vertical] of [[timeBar,false],[rowsBar,true]]) {
  const coordinate = event => {
    const box=element.getBoundingClientRect();
    return vertical ? event.clientY-box.top : event.clientX-box.left;
  };
  const axis=vertical ? 'scrollTop' : 'scrollLeft';
  let drag;
  function update(event) {
    if (!drag || event.pointerId!==drag.id || projectId!==drag.project) return;
    const move=coordinate(event)-drag.origin;
    if (drag.part===1) {
      timelineBody[axis]=drag.scroll+move/drag.bar*drag.world;
    } else if (vertical) {
      const result=TimelineView.verticalGrip(drag.part,drag.bar,drag.rows,drag.scroll-drag.rows.top,move);
      setRowView(result.layout,result.scroll);
    } else {
      const result=TimelineView.horizontalGrip(drag.part,drag.bar,drag.visible,drag.extent,drag.scale,drag.scroll,drag.frame,move,maxTimelineScale);
      setTimeView(result.scale,result.scroll);
    }
    drawTimelineBars();
  }
  element.onpointerdown=event=>{
    if (event.button!==0 || !timelineRows) return;
    const geometry=barGeometry(vertical);
    if (geometry.bar<=0) return;
    event.preventDefault();
    element.focus({preventScroll:true});
    const origin=coordinate(event);
    const part=TimelineView.partAt(origin,geometry.offset,geometry.drawn);
    if (part==null) timelineBody[axis]=(origin-geometry.drawn/2)/geometry.bar*geometry.world;
    drag={...geometry,id:event.pointerId,origin,part:part??1,project:projectId,scale:pxPerTick,
      frame:frameAt,rows:timelineRows,scroll:timelineBody[axis]};
    element.setPointerCapture(event.pointerId);
    element.dataset.hot=drag.part;
    element.dataset.dragging='';
    drawTimelineBars();
  };
  element.onpointermove=event=>{
    if (drag) {
      update(event);
      return;
    }
    if (!timelineRows) return;
    const {offset,drawn}=barGeometry(vertical);
    const part=TimelineView.partAt(coordinate(event),offset,drawn);
    if (part==null) delete element.dataset.hot;
    else element.dataset.hot=part;
  };
  function end(event) {
    if (event.pointerId!==drag?.id) return;
    drag=null;
    delete element.dataset.dragging;
    delete element.dataset.hot;
  }
  element.onpointerup=event=>{update(event);end(event);};
  element.onpointercancel=element.onlostpointercapture=end;
  element.onpointerleave=()=>{if (!drag) delete element.dataset.hot;};
  element.onkeydown=event=>{
    if (!timelineRows) return;
    const {key}=event;
    const extent=vertical ? timelineBody.clientHeight : timelineViewWidth();
    const direction=key===(vertical?'ArrowUp':'ArrowLeft') ? -1 : key===(vertical?'ArrowDown':'ArrowRight') ? 1 : 0;
    if (direction) timelineBody[axis]+=direction*(vertical ? timelineRows.height/2 : 40);
    else if (key==='PageUp' || key==='PageDown') timelineBody[axis]+=(key==='PageUp' ? -1 : 1)*extent;
    else if (key==='Home' || key==='End') timelineBody[axis]=key==='Home' ? 0 : vertical ? timelineBody.scrollHeight : timelineBody.scrollWidth;
    else if (key==='+' || key==='=' || key==='-') {
      const factor=key==='-' ? -1 : 1;
      if (vertical) resizeRows(timelineRows.height+factor*5);
      else zoomTimeline(1.4**factor);
    } else return;
    event.preventDefault();
    event.stopPropagation();
  };
}
function layoutViewer(center=false) {
  const roomWidth=viewerViewport.offsetWidth;
  const roomHeight=viewerViewport.offsetHeight;
  if (!project || !roomWidth || !roomHeight) return;
  const {width,height}=project.sequence;
  const scale=viewerZoom===null ? Math.min(roomWidth/width,roomHeight/height) : viewerZoom/(window.devicePixelRatio || 1);
  const pictureWidth=width*scale;
  const pictureHeight=height*scale;
  const scrolls=pictureWidth>roomWidth+.5 || pictureHeight>roomHeight+.5;
  const pastWidth=viewerPicture.offsetWidth;
  const pastHeight=viewerPicture.offsetHeight;
  const position=(scroll,viewport,offset,extent)=>Math.max(0,Math.min(1,(scroll+viewport/2-offset)/extent));
  const x=center || !pastWidth ? .5 : position(viewerViewport.scrollLeft,viewerViewport.clientWidth,viewerPicture.offsetLeft,pastWidth);
  const y=center || !pastHeight ? .5 : position(viewerViewport.scrollTop,viewerViewport.clientHeight,viewerPicture.offsetTop,pastHeight);
  viewerViewport.style.setProperty('--picture-width',pictureWidth.toFixed(2)+'px');
  viewerViewport.style.setProperty('--picture-height',pictureHeight.toFixed(2)+'px');
  viewerViewport.toggleAttribute('data-scrollable',scrolls);
  viewerViewport.tabIndex=scrolls ? 0 : -1;
  if (scrolls) {
    viewerViewport.scrollLeft=viewerPicture.offsetLeft+pictureWidth*x-viewerViewport.clientWidth/2;
    viewerViewport.scrollTop=viewerPicture.offsetTop+pictureHeight*y-viewerViewport.clientHeight/2;
  }
  const detail=Math.min(1,viewerZoom ?? 0);
  editorSurface.maxWidth=Math.max(1450,Math.round(width*detail));
  editorSurface.maxHeight=Math.max(900,Math.round(height*detail));
}
new ResizeObserver(()=>layoutViewer()).observe(viewerStage);
viewerViewport.onkeydown=event=>{
  if (event.target===viewerViewport && ['ArrowLeft','ArrowRight','ArrowUp','ArrowDown','PageUp','PageDown','Home','End'].includes(event.key)) {
    event.stopPropagation();
  }
};
byId('viewer-import').onclick=()=>byId('import').click();
byId('viewer-zoom').onchange = event => {
  viewerZoom=event.target.value==='fit' ? null : Number(event.target.value);
  layoutViewer();
  drawComposed();
};
const fullscreenToggle=byId('full-screen');
function viewerIsFullScreen() {
  return document.fullscreenElement===viewerPanel || viewerPanel.hasAttribute('data-viewer-fullscreen');
}
function updateViewerFullscreen() {
  const active=viewerIsFullScreen();
  const label=active ? 'Exit full screen viewer' : 'Full screen viewer';
  fullscreenToggle.setAttribute('aria-label',label);
  fullscreenToggle.setAttribute('aria-pressed',String(active));
  fullscreenToggle.title=label;
  fullscreenToggle.textContent=active ? '⤡' : '⤢';
  layoutViewer();
}
function exitViewerFullScreen() {
  if (document.fullscreenElement===viewerPanel) {
    document.exitFullscreen().catch(error=>status(error.message));
  } else if (viewerPanel.hasAttribute('data-viewer-fullscreen')) {
    viewerPanel.removeAttribute('data-viewer-fullscreen');
    updateViewerFullscreen();
  }
}
fullscreenToggle.onclick=async()=>{
  if (viewerIsFullScreen()) {
    exitViewerFullScreen();
    return;
  }
  if (viewerPanel.requestFullscreen) {
    try {
      await viewerPanel.requestFullscreen();
      return;
    } catch (_) {
      // The viewer still fills the browser when browser-level full screen is unavailable.
    }
  }
  if (byId('studio').hidden) return;
  viewerPanel.setAttribute('data-viewer-fullscreen','');
  updateViewerFullscreen();
  fullscreenToggle.focus({preventScroll:true});
};
document.addEventListener('fullscreenchange',updateViewerFullscreen);
byId('ruler').onpointerdown=beginRulerScrub;
byId('ruler').onpointermove=e=>{if (!e.buttons) skimTimeline(rulerValue(e));};
timelineBody.addEventListener('pointerleave',()=>clearSkim());
let rulerScrollFrame;
timelineBody.addEventListener('scroll',()=>{
  if (rulerScrollFrame!=null) return;
  rulerScrollFrame=requestAnimationFrame(()=>{
    rulerScrollFrame=null;
    drawRulerTicks();
    drawMarkerPanel();
    drawTimelineBars();
    scheduleTimelineMedia();
  });
});
new ResizeObserver(()=>{
  if (!timelineRows || !timelineBody.clientHeight || byId('studio').hidden) return;
  if (pxPerTick<minimumTimelineZoom()) {
    renderTimelineTracks();
    return;
  }
  const {height,video,audio,captions,top} = timelineRows;
  const scroll=timelineBody.scrollTop-top;
  setRowView(TimelineView.rows(height,timelineBody.clientHeight-TimelineView.ruler,video,audio,captions),scroll);
  drawRuler();
}).observe(timelineBody);
byId('timecode').addEventListener('change', event => {
  const at = fromTc(event.target.value);
  if (at !== null) seekTimeline(at);
});
function editPoint(forward) {
  const here=playheadTicks();
  const half=frameStep()/2;
  const marks=[...new Set([0,...project.sequence.markers.map(marker=>marker.frame),
    ...project.timeline.flatMap(clip=>[timelineStart(clip.id),timelineEnd(clip)])])].sort((a,b)=>a-b);
  const next=forward ? marks.find(at=>at>here+half) : marks.findLast(at=>at<here-half);
  if (next!=null) seekTimeline(next);
}
byId('prev-edit').onclick = () => editPoint(false);
byId('next-edit').onclick = () => editPoint(true);
function stepFrame(delta) { seekTimeline((sourcePreview ? ticks(preview.currentTime) : frameAt) + delta * frameStep()); }
byId('back-frame').onclick = () => stepFrame(-1);
byId('next-frame').onclick = () => stepFrame(1);

function bindPanelSplitter(id, property, bounds, observes) {
  const handle=byId(id);
  const host=handle.parentElement;
  const pane=byId(handle.getAttribute('aria-controls'));
  const height=handle.getAttribute('aria-orientation')==='horizontal';
  const size=()=>height ? pane.offsetHeight : pane.offsetWidth;
  const coordinate=event=>height ? -event.clientY : event.clientX;
  function preference() {
    const saved=Number(displayPreference(property,''));
    return Number.isFinite(saved) && saved>0 ? saved : null;
  }
  let desired=preference();
  let drag;
  const limit=(value,[min,max])=>Math.min(max,Math.max(min,Math.round(value)));
  function layout() {
    if (!handle.getClientRects().length) return;
    const range=bounds(host);
    const value=desired===null ? '' : limit(desired,range)+'px';
    if (host.style.getPropertyValue('--'+property)!==value) host.style.setProperty('--'+property,value);
    const actual=size();
    handle.setAttribute('aria-valuemin',String(Math.min(actual,range[0])));
    handle.setAttribute('aria-valuemax',String(Math.max(actual,range[1])));
    handle.setAttribute('aria-valuenow',String(actual));
    handle.setAttribute('aria-valuetext',actual+(height ? ' pixels tall' : ' pixels wide'));
  }
  function commit(value) {
    desired=value;
    storeDisplayPreference(property,value ?? '');
    layout();
  }
  handle.onpointerdown=event=>{
    if (event.button!==0 || drag) return;
    event.preventDefault();
    handle.focus({preventScroll:true});
    handle.setPointerCapture(event.pointerId);
    drag={id:event.pointerId,origin:coordinate(event),size:size(),prior:desired};
    handle.dataset.dragging='';
  };
  handle.onpointermove=event=>{
    if (event.pointerId!==drag?.id) return;
    desired=limit(drag.size+coordinate(event)-drag.origin,bounds(host));
    layout();
  };
  handle.onpointerup=event=>{
    if (event.pointerId!==drag?.id) return;
    drag=null;
    delete handle.dataset.dragging;
    commit(desired);
  };
  handle.onlostpointercapture=event=>{
    if (event.pointerId!==drag?.id) return;
    desired=drag.prior;
    drag=null;
    delete handle.dataset.dragging;
    layout();
  };
  handle.ondblclick=()=>commit(null);
  handle.onkeydown=event=>{
    if (event.metaKey || event.ctrlKey || event.altKey) return;
    const [min,max]=bounds(host);
    const more=height ? 'ArrowUp' : 'ArrowRight';
    const less=height ? 'ArrowDown' : 'ArrowLeft';
    const step=event.shiftKey ? 40 : 12;
    const sizes={Enter:null,Home:min,End:max,[more]:size()+step,[less]:size()-step};
    if (!Object.hasOwn(sizes,event.key)) return;
    event.preventDefault();
    event.stopPropagation();
    const value=sizes[event.key];
    commit(value===null ? null : limit(value,[min,max]));
  };
  const observer=new ResizeObserver(layout);
  for (const element of [host,...observes]) observer.observe(element);
  window.addEventListener('storage',event=>{
    if (event.key!=='editroom.'+property || drag) return;
    desired=preference();
    layout();
  });
}
const inspectorPanel=document.querySelector('.inspector');
bindPanelSplitter('media-splitter','media-panel-width',host=>{
  const style=getComputedStyle(host);
  const min=parseFloat(style.getPropertyValue('--media-min-width'));
  const inspectorWidth=inspectorPanel.offsetWidth;
  const gaps=parseFloat(style.columnGap)*(inspectorWidth ? 2 : 1);
  return [min,Math.max(min,Math.min(440,host.clientWidth-inspectorWidth-gaps-260))];
},[inspectorPanel]);
const editStatus=byId('edit-status');
bindPanelSplitter('timeline-splitter','timeline-panel-height',host=>{
  const style=getComputedStyle(host);
  const viewerMin=parseFloat(style.getPropertyValue('--viewer-min-height'));
  const gaps=2*parseFloat(style.rowGap);
  return [170,Math.max(170,host.clientHeight-viewerMin-gaps-editStatus.offsetHeight)];
},[editStatus]);

function updateDirectorPanel() {
  const open=directorEnabled && directorPanelOpen;
  const toggle=byId('toggle-director');
  toggle.hidden=!directorEnabled;
  toggle.setAttribute('aria-expanded',String(open));
  byId('director-panel').hidden=!open;
}
updateDirectorPanel();
byId('toggle-director').onclick = () => {
  // While a run works, the button is its Stop control, as in Editroom.
  if (byId('toggle-director').dataset.working==='true') return byId('stop-run').click();
  directorPanelOpen=!directorPanelOpen;
  updateDirectorPanel();
  storeDisplayPreference('director-open',String(directorPanelOpen));
};
byId('director-mode').onclick = () => mode(false);
byId('quick-mode').onclick = () => mode(true);
function mode(value) {
  quick = value;
  byId('director-mode').setAttribute('aria-pressed', String(!quick));
  byId('quick-mode').setAttribute('aria-pressed', String(quick));
  byId('prompt-ideas').hidden=quick;
  document.querySelector('label[for=prompt]').textContent = quick ? 'What should we make?' : 'What should change?';
}
for (const tab of document.querySelectorAll('.provider-tab')) tab.onclick=async()=>{
  const name=tab.dataset.name;
  if (name!=='local' && !providerProfile(name)) {
    openProviderSettings(name);
    return;
  }
  try {
    await post('/api/provider',{provider:name});
    byId('director-error').textContent='';
    await refresh();
  } catch (error) { byId('director-error').textContent=error.message; }
};
const editorSettings=byId('editor-settings-dialog');
const mediaLayoutSetting=byId('editor-media-layout');
const timeStyleSetting=byId('editor-time-style');
const autosaveSetting=byId('editor-autosave');
const savingError=byId('editor-saving-error');
const directorEnabledSetting=byId('editor-director-enabled');
const directorWorkSetting=byId('editor-director-work');
const attachedSetting=byId('attached-enabled');
const attachedUrl=byId('attached-url');
let attachedSettings;
let attachedRequest;
let attachedEditing=false;
let attachedCliEditing=false;
let attachedCliRegistration=null;
let attachedCliRegistrationRequest;
let attachedPoll;
function changeTimeStyle(value) {
  timeStyle=value==='simple' ? value : 'timecode';
  const field=byId('timecode');
  field.inputMode=timeStyle==='simple' ? 'text' : 'numeric';
  field.setAttribute('aria-label',timeStyle==='simple' ? 'Playhead time' : 'Playhead timecode');
  document.documentElement.dataset.editorTime=timeStyle;
  if (!project) return;
  field.value=tc(sourcePreview ? ticks(preview.currentTime) : frameAt);
  byId('total-time').textContent='/ '+tc(clipTotal());
  drawRuler();
  drawExportPreview();
}
const themeButtons=editorThemes.map(name=>{
  const button=item('button','button editor-theme','');
  button.type='button';
  button.dataset.appearance=name;
  const sample=item('span','theme-sample','');
  sample.setAttribute('aria-hidden','true');
  button.append(sample,item('span','',name[0].toUpperCase()+name.slice(1)));
  button.onclick=()=>{
    editorTheme=name;
    storeDisplayPreference('appearance',name);
    applyEditorTheme();
    syncEditorSettings();
  };
  byId('editor-themes').append(button);
  return button;
});
function syncEditorSettings() {
  for (const button of themeButtons) button.setAttribute('aria-pressed',String(button.dataset.appearance===editorTheme));
  mediaLayoutSetting.value=byId('media-list').dataset.list==='true' ? 'list' : 'thumbnails';
  timeStyleSetting.value=timeStyle;
  directorEnabledSetting.checked=directorEnabled;
  directorWorkSetting.disabled=!directorEnabled;
  directorWorkSetting.checked=showDirectorWork;
  if (!autosaveSetting.disabled) autosaveSetting.checked=state.autosave;
}
function openEditorSettings() {
  syncEditorSettings();
  editorSettings.showModal();
  renderAttachedSettings();
  refreshAttachedSettings();
  refreshAttachedCliRegistration();
  clearInterval(attachedPoll);
  attachedPoll=setInterval(refreshAttachedSettings,4000);
}
editorSettings.addEventListener('close',()=>{
  clearInterval(attachedPoll);
  attachedRequest?.abort();
  attachedCliRegistrationRequest?.abort();
  attachedCliRegistration=null;
  attachedStatus();
  savingError.hidden=true;
});
mediaLayoutSetting.onchange=()=>changeMediaLayout(mediaLayoutSetting.value==='list');
timeStyleSetting.onchange=()=>{
  changeTimeStyle(timeStyleSetting.value);
  storeDisplayPreference('time-style',timeStyle);
};
autosaveSetting.onchange=async()=>{
  const autosave=autosaveSetting.checked;
  autosaveSetting.disabled=true;
  savingError.hidden=true;
  try {
    await post('/api/settings/saving',{autosave});
    await refresh();
  } catch (error) {
    savingError.textContent=error.message;
    savingError.hidden=false;
  } finally {
    autosaveSetting.disabled=false;
    autosaveSetting.checked=state.autosave;
  }
};
directorEnabledSetting.onchange=()=>{
  directorEnabled=directorEnabledSetting.checked;
  directorWorkSetting.disabled=!directorEnabled;
  updateDirectorPanel();
  storeDisplayPreference('director-enabled',String(directorEnabled));
};
function changeDirectorWork(visible) {
  if (visible===showDirectorWork) return;
  showDirectorWork=visible;
  if (!visible) directorAnimation?.abort();
  if (project && !byId('studio').hidden) renderStudio(state);
}
directorWorkSetting.onchange=()=>{
  changeDirectorWork(directorWorkSetting.checked);
  storeDisplayPreference('director-work',String(showDirectorWork));
};
window.addEventListener('storage',event=>{
  if (event.key==='editroom.director-enabled') {
    directorEnabled=event.newValue!=='false';
    directorEnabledSetting.checked=directorEnabled;
    directorWorkSetting.disabled=!directorEnabled;
    updateDirectorPanel();
  }
  if (event.key==='editroom.director-open') {
    directorPanelOpen=event.newValue!=='false';
    updateDirectorPanel();
  }
  if (event.key==='editroom.director-work') {
    changeDirectorWork(event.newValue!=='false');
    directorWorkSetting.checked=showDirectorWork;
  }
});

function attachedStatus(message='',failed=false) {
  const notice=byId('attached-status');
  notice.textContent=message;
  notice.hidden=!message;
  notice.classList.toggle('error',failed);
}

function localAttachedClient() {
  return !accountMode && location.protocol==='http:' && (location.hostname==='localhost' || location.hostname==='[::1]' || /^127\./.test(location.hostname));
}

function renderAttachedSettings() {
  const enabled=Boolean(attachedSettings?.access.enabled);
  attachedSetting.checked=enabled;
  attachedSetting.disabled=attachedEditing || !attachedSettings;
  byId('attached-connection').hidden=!enabled;
  const endpoint=enabled && projectId ? new URL(projectUrl('/mcp'),location.origin).href : '';
  if (attachedUrl.value!==endpoint) attachedUrl.value=endpoint;
  byId('attached-copy-setup').disabled=attachedEditing;
  byId('attached-download-claude').disabled=attachedEditing || !endpoint;
  byId('attached-copy-stdio').disabled=attachedEditing;
  byId('attached-copy-key').disabled=attachedEditing;
  const local=localAttachedClient();
  for (const id of ['attached-cli-connect','attached-cli-remove']) byId(id).hidden=!local;
  for (const id of ['attached-cli-client','attached-cli-connect','attached-cli-remove','attached-copy-install','attached-copy-remove']) {
    byId(id).disabled=attachedEditing || attachedCliEditing || !endpoint;
  }
  const connect=byId('attached-cli-connect');
  const remove=byId('attached-cli-remove');
  connect.disabled ||= attachedCliRegistration===null || ['registered','different'].includes(attachedCliRegistration);
  remove.disabled ||= attachedCliRegistration===null || attachedCliRegistration==='missing';
  const registration=byId('attached-cli-status');
  registration.hidden=!local || !endpoint;
  const states={
    registered:'Registered on this computer',
    missing:'Ready to connect',
    different:'Registered with different settings',
    unavailable:'Registration status unavailable'
  };
  registration.textContent=states[attachedCliRegistration] || 'Checking registration…';
  const names=attachedSettings?.active.map(agent=>agent.name) || [];
  const active=byId('attached-active');
  active.hidden=!names.length;
  active.textContent=names.length ? 'Recently active · '+names.join(', ') : '';
}

async function refreshAttachedCliRegistration() {
  attachedCliRegistrationRequest?.abort();
  if (!editorSettings.open || !localAttachedClient() || !attachedSettings?.access.enabled || !projectId) return;
  const controller=new AbortController();
  attachedCliRegistrationRequest=controller;
  attachedCliRegistration=null;
  renderAttachedSettings();
  const client=byId('attached-cli-client').value;
  try {
    const result=await api(projectUrl('/mcp-cli-registration')+'?client='+encodeURIComponent(client),{signal:controller.signal});
    if (!controller.signal.aborted) attachedCliRegistration=result;
  } catch (_) {
    if (!controller.signal.aborted) attachedCliRegistration='unavailable';
  } finally {
    if (attachedCliRegistrationRequest===controller) attachedCliRegistrationRequest=null;
    if (!controller.signal.aborted) renderAttachedSettings();
  }
}
byId('attached-cli-client').onchange=()=>refreshAttachedCliRegistration();

async function refreshAttachedSettings() {
  if (attachedRequest || attachedEditing) return;
  const controller=new AbortController();
  attachedRequest=controller;
  try {
    const result=await api('/api/attached-agents',{signal:controller.signal});
    if (!controller.signal.aborted) {
      const newlyEnabled=result.access.enabled && !attachedSettings?.access.enabled;
      attachedSettings=result;
      renderAttachedSettings();
      if (newlyEnabled) refreshAttachedCliRegistration();
    }
  } catch (error) {
    if (!controller.signal.aborted && editorSettings.open) attachedStatus(error.message,true);
  } finally {
    if (attachedRequest===controller) attachedRequest=null;
  }
}

attachedSetting.onchange=async()=>{
  const enabled=attachedSetting.checked;
  attachedEditing=true;
  attachedRequest?.abort();
  attachedCliRegistrationRequest?.abort();
  attachedCliRegistration=null;
  attachedSetting.disabled=true;
  byId('attached-copy-setup').disabled=true;
  byId('attached-download-claude').disabled=true;
  byId('attached-copy-stdio').disabled=true;
  byId('attached-copy-key').disabled=true;
  for (const id of ['attached-cli-client','attached-cli-connect','attached-cli-remove','attached-copy-install','attached-copy-remove']) byId(id).disabled=true;
  attachedStatus(enabled ? 'Enabling attached agents…' : 'Disconnecting attached agents…');
  try {
    attachedSettings=await post('/api/attached-agents',{enabled});
    attachedStatus(enabled ? 'Attached agents can connect to this studio.' : 'Attached agents disconnected.');
    if (enabled) refreshAttachedCliRegistration();
  } catch (error) {
    attachedStatus(error.message,true);
  } finally {
    attachedEditing=false;
    renderAttachedSettings();
  }
};
attachedUrl.onclick=()=>attachedUrl.select();
byId('attached-copy-setup').onclick=async()=>{
  if (!attachedSettings?.access.enabled) return;
  const setup=`Connect to EditCastle over Streamable HTTP MCP at ${attachedUrl.value}. Add the EditCastle agent key from Editor Settings as a bearer credential in your MCP client. Then call get_project and list_media to read this project before editing.`;
  try {
    await navigator.clipboard.writeText(setup);
    attachedStatus('Setup copied.');
  } catch (_) {
    attachedUrl.select();
    attachedStatus('Copy the selected endpoint and configure Streamable HTTP with the agent key.',true);
  }
};
byId('attached-download-claude').onclick=()=>{
  if (!attachedSettings?.access.enabled || !projectId) return;
  location.assign(projectUrl('/mcp-bundle'));
};
async function copyAttachedCliCommand(action) {
  if (!attachedSettings?.access.enabled || !projectId) return;
  attachedStatus();
  const client=byId('attached-cli-client').value;
  const request=api(projectUrl('/mcp-cli-setup')+'?client='+encodeURIComponent(client));
  try {
    let setup;
    if (typeof ClipboardItem==='function' && navigator.clipboard?.write) {
      const command=request.then(result=>new Blob([result[action]],{type:'text/plain'}));
      [setup]=await Promise.all([request,navigator.clipboard.write([new ClipboardItem({'text/plain':command})])]);
    } else {
      setup=await request;
      await navigator.clipboard.writeText(setup[action]);
    }
    attachedStatus(action==='install' && setup.needs_paths ?
      'Command copied. Set the bridge and key-file paths on the client before running it.' :
      'Command copied. Run it in Terminal on the client.');
  } catch (error) {
    attachedStatus(error.message || 'The command could not be copied.',true);
  }
}
byId('attached-copy-install').onclick=()=>copyAttachedCliCommand('install');
byId('attached-copy-remove').onclick=()=>copyAttachedCliCommand('remove');
async function changeAttachedCli(action) {
  if (!attachedSettings?.access.enabled || !projectId || attachedCliEditing) return;
  attachedCliEditing=true;
  renderAttachedSettings();
  const client=byId('attached-cli-client');
  const label=client.selectedOptions[0].textContent;
  attachedStatus(action==='connect' ? 'Connecting '+label+'…' : 'Removing the '+label+' entry…');
  try {
    await post(projectUrl('/mcp-cli-setup'),{client:client.value,action});
    attachedStatus(action==='connect' ? 'Project registered with '+label+'.' : 'Project removed from '+label+'.');
  } catch (error) {
    attachedStatus(error.message,true);
  } finally {
    attachedCliEditing=false;
    renderAttachedSettings();
    await refreshAttachedCliRegistration();
  }
}
byId('attached-cli-connect').onclick=()=>changeAttachedCli('connect');
byId('attached-cli-remove').onclick=()=>changeAttachedCli('remove');
byId('attached-copy-stdio').onclick=async()=>{
  if (!attachedSettings?.access.enabled || !attachedUrl.value) return;
  const setup={mcpServers:{editroom:{
    command:'/absolute/path/to/editcastle-web',
    args:['mcp-stdio',attachedUrl.value],
    env:{EDITROOM_MCP_KEY_FILE:'/absolute/path/to/editroom-agent-key'}
  }}};
  try {
    await navigator.clipboard.writeText(JSON.stringify(setup,null,2));
    attachedStatus('Stdio configuration copied. Set the executable and agent key file paths on the client.');
  } catch (_) {
    attachedStatus('Clipboard access is unavailable. Use the MCP endpoint with the editcastle-web mcp-stdio command.',true);
  }
};
byId('attached-copy-key').onclick=async()=>{
  if (!attachedSettings?.access.enabled) return;
  try {
    if (typeof ClipboardItem==='function' && navigator.clipboard?.write) {
      const key=api('/api/attached-agents/key',{method:'POST',data:{},raw:true}).then(blob=>new Blob([blob],{type:'text/plain'}));
      await Promise.all([key,navigator.clipboard.write([new ClipboardItem({'text/plain':key})])]);
    } else {
      const key=await api('/api/attached-agents/key',{method:'POST',data:{},raw:true});
      await navigator.clipboard.writeText(await key.text());
    }
    attachedStatus('Agent key copied.');
  } catch (_) {
    attachedStatus('Clipboard access is unavailable. Use the agent key configured on the studio host.',true);
  }
};
changeTimeStyle(timeStyle);

const shortcutDialog = byId('shortcut-dialog');
const shortcutSearch = byId('shortcut-search');
let recordingShortcut;
const shortcutButtons = {
  new:['file-new','new-project'],open:['file-open'],save:['file-save'],saveAs:['file-save-as'],
  importMedia:['file-import'],export:['export'],undo:['undo'],redo:['redo'],
  playPause:['play-cut'],stepBack:['back-frame'],stepForward:['next-frame'],
  previousEdit:['prev-edit'],nextEdit:['next-edit'],loop:['loop'],fullScreenViewer:['full-screen'],
  selectTool:['select-tool'],cutTool:['cut-tool'],split:['split'],copyGrade:['copy-grade'],
  pasteGrade:['paste-grade'],delete:['remove'],addText:['new-title'],
  zoomIn:['zoom-timeline-in'],zoomOut:['zoom-timeline-out'],zoomToFit:['fit-timeline'],
  snapping:['snap'],sticky:['sticky'],skimming:['skim'],audioScrubbing:['audio-scrub']
};

function updateShortcutHints() {
  for (const action of StudioShortcuts.definitions) {
    const keys = shortcutPreferences.combos(action).map(StudioShortcuts.keyboardShortcut).join(' ');
    for (const id of shortcutButtons[action.id] || []) {
      const button = byId(id);
      if (keys) button.setAttribute('aria-keyshortcuts',keys);
      else button.removeAttribute('aria-keyshortcuts');
    }
  }
}

function openShortcutSettings() {
  shortcutSearch.value = '';
  recordingShortcut = null;
  renderShortcutSettings();
  shortcutDialog.showModal();
  shortcutSearch.focus();
}

function renderShortcutSettings(note,focus) {
  const query = shortcutSearch.value.trim().toLowerCase();
  const sections = new Map();
  const matches = (action,combos)=>!query || action.title.toLowerCase().includes(query) ||
    combos.some(combo=>StudioShortcuts.display(combo).toLowerCase().includes(query));
  const add = (group,node)=>{
    if (!sections.has(group)) {
      const section = item('section','shortcut-section','');
      section.append(item('h3','',group));
      sections.set(group,section);
    }
    sections.get(group).append(node);
  };
  const rowButton = (text,label,onclick) => {
    const button = item('button','button',text);
    button.type = 'button';
    button.setAttribute('aria-label',label);
    button.onclick = onclick;
    return button;
  };
  for (const action of StudioShortcuts.definitions) {
    const combos = shortcutPreferences.combos(action);
    if (matches(action,combos)) {
      const row = item('div','shortcut-row','');
      const record = rowButton(recordingShortcut===action ? 'Press a key…' : combos.map(StudioShortcuts.display).join(' / ') || 'Unassigned',
        'Change '+action.title,()=>{
          recordingShortcut = recordingShortcut===action ? null : action;
          renderShortcutSettings(undefined,action.id);
        });
      record.classList.add('shortcut-key');
      record.dataset.shortcut = action.id;
      record.dataset.recording = String(recordingShortcut===action);
      const reset = rowButton('Reset','Reset '+action.title,()=>{
        shortcutPreferences.reset(action);
        recordingShortcut = null;
        renderShortcutSettings(undefined,action.id);
      });
      reset.disabled = !Object.hasOwn(shortcutPreferences.overrides,action.id);
      const clear = rowButton('Clear','Clear '+action.title,()=>{
        shortcutPreferences.clear(action);
        recordingShortcut = null;
        renderShortcutSettings(undefined,action.id);
      });
      clear.disabled = !combos.length;
      row.append(item('span','',action.title),record,reset,clear);
      add(action.group,row);
    }
    if (action.id==='playPause' && (matches(action,combos) ||
        ['skimming','skimmer','playhead'].some(label=>query.includes(label) || label.startsWith(query)))) {
      const row = item('label','shortcut-skim','While skimming, play from');
      const dropdown = item('select','','');
      dropdown.setAttribute('aria-label','While skimming, play from');
      dropdown.append(option('Playhead','playhead'),option('Skimmer','skimmer'));
      dropdown.value = shortcutPreferences.playFromSkimmer ? 'skimmer' : 'playhead';
      dropdown.onchange = () => shortcutPreferences.setSkim(dropdown.value==='skimmer');
      row.append(dropdown);
      add(action.group,row);
    }
  }
  const list = byId('shortcut-list');
  const scroll = list.scrollTop;
  list.replaceChildren(...(sections.size ? sections.values() : [item('p','empty','No shortcuts match your search.')]));
  list.scrollTop = scroll;
  byId('shortcut-status').textContent = note || (recordingShortcut
    ? 'Press the new shortcut, or Escape to cancel.' : 'Select a shortcut to change it.');
  byId('shortcut-restore').disabled = !Object.keys(shortcutPreferences.overrides).length;
  updateShortcutHints();
  if (focus) (list.querySelector('[data-shortcut="'+focus+'"]') || shortcutSearch).focus({preventScroll:true});
}

shortcutSearch.oninput = () => renderShortcutSettings();
byId('shortcut-restore').onclick = () => {
  recordingShortcut = null;
  shortcutPreferences.resetAll();
  renderShortcutSettings('Restored the default shortcuts.');
};
shortcutDialog.addEventListener('close',()=>{recordingShortcut=null;});
shortcutDialog.addEventListener('keydown',event=>{
  if (!recordingShortcut) return;
  event.preventDefault();
  event.stopPropagation();
  const action = recordingShortcut;
  if (event.key==='Escape') {
    recordingShortcut = null;
    renderShortcutSettings('Assignment canceled.',action.id);
    return;
  }
  const combo = StudioShortcuts.fromEvent(event);
  if (!combo || event.repeat) return;
  recordingShortcut = null;
  const taken = shortcutPreferences.set(action,combo);
  const label = StudioShortcuts.display(combo);
  renderShortcutSettings(taken ? label+' moved from '+taken.title+' to '+action.title+'.' : label+' assigned to '+action.title+'.',action.id);
});
window.addEventListener('storage',event=>{
  if (event.key && event.key!==StudioShortcuts.preferenceKey && event.key!==StudioShortcuts.skimKey) return;
  shortcutPreferences.load();
  if (shortcutDialog.open) renderShortcutSettings();
  else updateShortcutHints();
});
updateShortcutHints();

const shortcutActions = {
  new:()=>projectDialog(false),open:openRoomDialog,save:saveProjectVersion,saveAs:openSaveAs,
  importMedia:()=>byId('import').click(),export:()=>byId('export').click(),
  undo:()=>mcp('history',{action:'undo'}),redo:()=>mcp('history',{action:'redo'}),
  playPause:()=>byId('play-cut').click(),stepBack:()=>stepFrame(-1),stepForward:()=>stepFrame(1),
  previousEdit:()=>editPoint(false),nextEdit:()=>editPoint(true),
  goToStart:()=>seekTimeline(0),goToEnd:()=>seekTimeline(clipTotal()),
  loop:()=>byId('loop').click(),fullScreenViewer:()=>byId('full-screen').click(),
  selectTool:()=>byId('select-tool').click(),cutTool:()=>byId('cut-tool').click(),
  split:splitSelection,addDissolve:()=>addDissolve(),detachAudio:detachSelection,duplicate:duplicateSelection,
  freezeFrame:()=>freezeSelected(),reverse:()=>reverseSelected(),
  copyGrade:copySelectedGrade,pasteGrade:pasteSelectedGrade,
  delete:()=>byId('remove').click(),rippleDelete:()=>deletePicked(true),
  rippleTrimStart:()=>rippleFromPlayhead('start'),rippleTrimEnd:()=>rippleFromPlayhead('end'),
  addText:()=>addGraphic('add_title',{text:'Text'}),
  addMarker:addMarkerAtPlayhead,setIn:()=>markAtPlayhead('in'),setOut:()=>markAtPlayhead('out'),
  clearInOut:()=>saveRange({clear:true}),
  zoomIn:()=>zoomTimeline(1.4),zoomOut:()=>zoomTimeline(1/1.4),zoomToFit:()=>zoomTimeline(null),
  snapping:()=>byId('snap').click(),sticky:()=>byId('sticky').click(),skimming:()=>byId('skim').click(),
  audioScrubbing:()=>byId('audio-scrub').click()
};
function rippleFromPlayhead(side) {
  return mcp('ripple_trim',{...selectionArgs(),side,at:playheadTicks()/TICKS_PER_SECOND});
}
const clipboardActions = {
  a:()=>captionSelection.size || captionFocus ? setCaptionSelection(captions.map(caption=>caption.id))
    : setSelection(project.timeline.map(clip=>clip.id)),
  c:()=>copySelection(),x:()=>copySelection(true),v:pasteSelection
};
document.addEventListener('keydown', event => {
  if (event.defaultPrevented || byId('studio').hidden || document.querySelector('dialog[open]')) return;
  const target = event.target;
  if (target.closest('#export-dialog')) return;
  const command = event.metaKey || event.ctrlKey;
  const combo = StudioShortcuts.fromEvent(event);
  const bound = combo && shortcutPreferences.action(combo);
  if (!bound && command && !event.altKey && !event.shiftKey && event.key===',') {
    event.preventDefault();
    openShortcutSettings();
    return;
  }
  const typing = target.closest('input,select,textarea,[contenteditable],.grade-wheel');
  if (event.key==='Escape' && !command && !event.altKey && !event.shiftKey && byId('clip-menu').hidden) {
    if (viewerIsFullScreen()) {
      event.preventDefault();
      exitViewerFullScreen();
      fullscreenToggle.focus({preventScroll:true});
      return;
    }
    const stop=byId('stop-run');
    if (directorWatch && !stop.hidden && !stop.disabled) {event.preventDefault();stop.click();return;}
    if (typing) return;
    event.preventDefault();
    if (document.fullscreenElement) {document.exitFullscreen();return;}
    clearLibrarySelection();
    setSelection([]);
    shortcutActions.selectTool();
    return;
  }
  const activatesControl = !command && !event.altKey && ['Space','Enter'].includes(event.code) &&
    target.closest('button:not([data-clip]):not([data-caption]),summary');
  if (directorOwnsCut()) {
    if (typing || activatesControl) return;
    if (bound) event.preventDefault();
    if (bound?.id==='playPause' && !event.repeat && !byId('play-cut').disabled) toggleViewerPlayback();
    return;
  }
  if (typing || activatesControl) return;
  if (!combo) return;
  const action = bound ? shortcutActions[bound.id] : (command && !event.altKey && !event.shiftKey &&
    target.closest('#timeline-body') ? clipboardActions[combo.key] : null);
  if (action) {event.preventDefault();action();}
});
mode(false);

// The Director show. While a run works, the timeline stays as saved and a thinking overlay scans
// the media and sketches the coming cut. When the run lands, a swarm of cursors performs it.
let thinkingShow=null;
let thinkingRun=null;
let thinkingNoted=0;
let swarmShow=null;
const calmMotion=()=>matchMedia('(prefers-reduced-motion: reduce)').matches;
const laneNamed=name=>[...timelineBody.querySelectorAll('[data-track]')].find(node=>node.dataset.track===name);
const clipNode=id=>[...timelineBody.querySelectorAll('[data-clip]')].find(node=>node.dataset.clip===id);
const tileNode=id=>[...document.querySelectorAll('#media-list [data-asset]')].find(node=>node.dataset.asset===id);
function within(rect,frame) {
  return rect.width>0 && rect.right>frame.left+2 && rect.left<frame.right-2 && rect.bottom>frame.top+2 && rect.top<frame.bottom-2;
}
function clipped(rect,frame) {
  const left=Math.max(rect.left,frame.left);
  const top=Math.max(rect.top,frame.top);
  return {left,top,width:Math.min(rect.right,frame.right)-left,height:Math.min(rect.bottom,frame.bottom)-top};
}
function thinkingTargets(stage) {
  const list=byId('media-list').getBoundingClientRect();
  const tiles=[...document.querySelectorAll('#media-list [data-asset]')].map(node=>({key:'asset:'+node.dataset.asset,rect:node.getBoundingClientRect()}))
    .filter(target=>within(target.rect,list)).map(target=>({...target,rect:clipped(target.rect,list)}));
  if (stage==='looking') return tiles;
  const body=timelineBody.getBoundingClientRect();
  const clips=[...timelineBody.querySelectorAll('[data-clip]')].map(node=>({key:'clip:'+node.dataset.clip,rect:node.getBoundingClientRect()}))
    .filter(target=>within(target.rect,body)).map(target=>({...target,rect:clipped(target.rect,body)}));
  return [...clips,...tiles,{key:'viewer',rect:byId('viewer-picture').getBoundingClientRect()}];
}
function beginThinking(run) {
  if (thinkingRun===run.id || !showDirectorWork || calmMotion() || !window.MagicShow) return;
  endThinking();
  swarmShow?.skip();
  thinkingRun=run.id;
  thinkingNoted=0;
  const tint={claude:[217,120,87],chatgpt:[15,163,128]}[state.provider] || [242,242,242];
  thinkingShow=MagicShow.thinking({tint,targets:thinkingTargets,
    viewer:()=>byId('viewer-picture').getBoundingClientRect(),
    cutouts:()=>[{rect:byId('director-panel').getBoundingClientRect(),radius:18},
      {rect:byId('toggle-director').getBoundingClientRect(),radius:99,grow:3}]});
}
function endThinking() {
  thinkingShow?.end();
  thinkingShow=null;
  thinkingRun=null;
}
function noteThinking(events) {
  if (!thinkingShow) return;
  for (const event of events.slice(thinkingNoted)) {
    const text=String(event);
    if (/plac|trim|mov|split|grad|mix|transition|title|remov|delet|arrang|mark|laying|applying/i.test(text)) thinkingShow.advance('thinking');
    else if (/review|analy|check|look|read|study|search|listen|transcri|finding|choosing/i.test(text)) {
      thinkingShow.advance('analyzing');
      const named=savedProject.sources.filter(asset=>asset.title && text.includes(asset.title)).sort((a,b)=>b.title.length-a.title.length)[0];
      if (named) {
        thinkingShow.focus('asset:'+named.id);
        for (const clip of savedProject.timeline) if (clip.source===named.id) thinkingShow.focus('clip:'+clip.id);
      } else thinkingShow.focus('viewer');
    }
  }
  thinkingNoted=events.length;
}
function sketchDraft(edits) {
  thinkingShow.advance('thinking');
  for (const step of edits) {
    if (step.after==null || step.track_id || step.ruler_item || !step.after_width) continue;
    const {track,after,after_width:width}=step;
    thinkingShow.plan(step.clip,()=>{
      const lane=laneNamed(track)?.getBoundingClientRect();
      const body=timelineBody.getBoundingClientRect();
      if (!lane) return null;
      const rect={left:lane.left+after*pxPerTick,top:lane.top,width:width*pxPerTick,height:lane.height};
      rect.right=rect.left+rect.width;
      rect.bottom=rect.top+rect.height;
      return within(rect,body) ? clipped(rect,body) : null;
    });
  }
}
function swarmTasks(steps) {
  const show=thinkingShow;
  const places=steps.filter(step=>step.before==null && step.after!=null && !step.ruler_item && !step.track_id).sort((a,b)=>a.after-b.after);
  const removals=steps.some(step=>step.after==null);
  const shifts=steps.some(step=>step.before!=null && step.after!=null && (step.before!==step.after || step.before_width!==step.after_width));
  const overlay=item('div','magic-swarm','');
  overlay.setAttribute('aria-hidden','true');
  document.body.append(overlay);
  const tasks=steps.map(step=>{
    const lane=()=>(step.ruler_item ? byId('ruler') : step.track_id
      ? timelineBody.querySelector('[data-track-header="'+step.track_id+'"]') : laneNamed(step.track))?.getBoundingClientRect();
    const at=(ticks,box)=>box.left+ticks*pxPerTick;
    const middle=box=>box.top+box.height/2;
    const node=()=>clipNode(step.clip);
    const mark=(...names)=>({begin:()=>node()?.classList.add('magic-hot',...names),cool:()=>node()?.classList.remove('magic-hot',...names)});
    if (step.ruler_item || step.track_id) {
      return {kind:'change',label:step.label || 'Edit',delay:.6+Math.random()*.4,points:()=>{
        const box=lane();
        if (!box) return null;
        const spot={x:step.track_id ? box.left+Math.min(box.width/2,60) : at(step.after ?? step.before ?? 0,box),y:middle(box)};
        return {grab:spot,release:spot};
      }};
    }
    if (step.after==null) {
      let block;
      return {kind:'remove',label:'Delete',delay:Math.random()*.2,points:()=>{
        const box=laneNamed(step.from_track || step.track)?.getBoundingClientRect() || timelineBody.getBoundingClientRect();
        const width=(step.before_width || 0)*pxPerTick;
        const spot={x:at(step.before || 0,box)+width/2,y:middle(box)};
        if (!block) {
          block=item('span','magic-removed',step.label || '');
          Object.assign(block.style,{left:at(step.before || 0,box)+'px',top:box.top+1+'px',width:Math.max(6,width)+'px',height:box.height-2+'px'});
          overlay.append(block);
        }
        return {grab:spot,release:spot};
      },finish:()=>block?.remove()};
    }
    if (step.before==null) {
      const asset=source(step.source);
      const order=places.indexOf(step);
      const tile=()=>{
        const found=tileNode(step.source)?.getBoundingClientRect();
        const list=byId('media-list').getBoundingClientRect();
        if (found && within(found,list)) return found;
        return {left:list.left+list.width/2-32,top:found && found.top>list.bottom ? list.bottom-30 : list.top+2,width:64,height:36};
      };
      const start=tile();
      return {kind:'place',label:asset && asset.audio && !asset.video ? 'Music' : 'Place',
        delay:(removals || shifts ? .5 : 0)+order*.09+Math.random()*.25,
        carry:{from:{width:start.width,height:start.height},
          to:{width:Math.min(Math.max(8,step.after_width*pxPerTick),260),height:(laneNamed(step.track)?.getBoundingClientRect().height || 40)-2},
          image:asset?.video ? sourceUrl('thumb',asset.id,asset.version) : ''},
        points:()=>{
          const box=lane();
          if (!box) return null;
          const from=tile();
          return {grab:{x:from.left+from.width/2,y:from.top+from.height/2},
            release:{x:at(step.after,box)+Math.min(step.after_width*pxPerTick,260)*.3,y:middle(box)}};
        },
        begin:()=>{},
        finish:()=>{
          node()?.classList.remove('magic-waiting');
          node()?.classList.add('magic-hot');
          show?.landed(step.clip);
        },
        cool:()=>node()?.classList.remove('magic-hot')};
    }
    const [before,after,oldWidth,newWidth]=[step.before,step.after,step.before_width ?? step.after_width,step.after_width];
    const moved=before!==after || (step.from_track && step.from_track!==step.track);
    const sized=oldWidth!==newWidth;
    if (!moved && !sized) {
      return {plain:true,kind:'change',label:'Edit',delay:(places.length ? 1 : .3)+Math.random()*.5,...mark(),points:()=>{
        const box=lane();
        if (!box) return null;
        const spot={x:at(after,box)+Math.min(newWidth*pxPerTick/2,80),y:middle(box)};
        return {grab:spot,release:spot};
      }};
    }
    const head=sized && before!==after && before+oldWidth===after+newWidth;
    const tail=sized && before===after;
    const rise=()=>{
      const from=laneNamed(step.from_track || step.track)?.getBoundingClientRect();
      const to=lane();
      return from && to ? from.top-to.top : 0;
    };
    const pose=u=>{
      const clip=node();
      if (!clip) return;
      if (u>=1) {
        clip.style.transform='';
        clip.style.width=clip.dataset.magicWidth ?? clip.style.width;
        delete clip.dataset.magicWidth;
        return;
      }
      clip.dataset.magicWidth ??= clip.style.width;
      clip.style.transform='translate('+(before-after)*pxPerTick*(1-u)+'px,'+(u<.5 ? rise() : 0)+'px)';
      if (sized) clip.style.width=Math.max(2,(oldWidth+(newWidth-oldWidth)*u)*pxPerTick)+'px';
    };
    return {kind:head || tail ? 'trim' : 'move',label:head || tail || !moved ? 'Trim' : 'Move',delay:.15+Math.random()*.4,
      ...mark(head ? 'magic-trim-head' : tail ? 'magic-trim-tail' : 'magic-moving'),apply:pose,finish:()=>pose(1),
      points:()=>{
        const box=lane();
        if (!box) return null;
        const y=middle(box);
        if (tail) return {grab:{x:at(before+oldWidth,box),y},release:{x:at(after+newWidth,box),y}};
        if (head) return {grab:{x:at(before,box),y},release:{x:at(after,box),y}};
        return {grab:{x:at(before,box)+oldWidth*pxPerTick/2,y:y+rise()},release:{x:at(after,box)+newWidth*pxPerTick/2,y}};
      }};
  });
  const cards=gradeCards(overlay,steps.filter(step=>step.detail?.grade && step.after!=null).length);
  const extra=steps.flatMap((step,index)=>detailTasks(step,tasks[index],overlay,cards));
  const detailed=new Set(extra.map(task=>task.clip));
  return {tasks:[...tasks.filter((task,index)=>!(task.plain && detailed.has(steps[index].clip))),...extra],overlay};
}
// The Grade cards the swarm works on, stacked over the inspector.
function gradeCards(overlay,count) {
  const panel=()=>{
    const box=document.querySelector('.panel.inspector').getBoundingClientRect();
    if (box.width>=160) return box;
    // No inspector on screen: the cards take the right edge of the viewer instead.
    const viewer=byId('viewer-panel').getBoundingClientRect();
    return {left:viewer.right-272,top:viewer.top,width:272,height:viewer.height};
  };
  let made=0;
  let open=0;
  let backdrop;
  const frame=slot=>{
    const box=panel();
    const slots=Math.max(2,Math.floor((box.height-60)/102));
    return {left:box.left+12,top:box.top+52+(slot%slots)*102,width:box.width-24,height:96};
  };
  return {frame,next:()=>made++,
    show(card) {
      if (!backdrop) {
        const box=panel();
        backdrop=item('div','magic-grade-back','');
        backdrop.append(item('strong','','Grade'),item('span','',count+(count===1 ? ' clip' : ' clips')));
        Object.assign(backdrop.style,{left:box.left+'px',top:box.top+'px',width:box.width+'px',height:box.height+'px'});
        overlay.prepend(backdrop);
        requestAnimationFrame(()=>backdrop.classList.add('shown'));
      }
      open++;
      overlay.append(card);
      requestAnimationFrame(()=>card.classList.add('shown'));
    },
    hide(card,toward) {
      const box=card.getBoundingClientRect();
      card.style.transform='translate('+(toward.x-box.left-box.width/2)+'px,'+(toward.y-box.top-box.height/2)+'px) scale(.3)';
      card.classList.remove('shown');
      setTimeout(()=>card.remove(),180);
      if (--open<=0 && made>=count) setTimeout(()=>backdrop?.classList.remove('shown'),300);
    }};
}
function detailTasks(step,main,overlay,cards) {
  const detail=step.detail;
  if (!detail || step.after==null) return [];
  const after=main.plain ? null : main;
  const wait=main.plain ? .3+Math.random()*.5 : 0;
  const box=()=>{
    const found=clipNode(step.clip)?.getBoundingClientRect();
    if (found?.width) return found;
    const lane=laneNamed(step.track)?.getBoundingClientRect();
    return lane && {left:lane.left+step.after*pxPerTick,top:lane.top,width:step.after_width*pxPerTick,height:lane.height,
      right:lane.left+(step.after+step.after_width)*pxPerTick,bottom:lane.bottom};
  };
  const hot={begin:()=>clipNode(step.clip)?.classList.add('magic-hot'),cool:()=>clipNode(step.clip)?.classList.remove('magic-hot')};
  const base={clip:step.clip,after,...hot};
  // Fade lengths are stored in timeline ticks.
  const frames=count=>count*pxPerTick;
  const tasks=[];
  if (detail.typed) {
    const words=detail.typed;
    const face=()=>clipNode(step.clip)?.querySelector('strong');
    tasks.push({...base,kind:'type',seconds:.12+.045*words.length,delay:wait+.05,
      label:raw=>words.slice(0,Math.ceil(raw*words.length))+'|',
      progress:raw=>{ const node=face(); if (node) node.textContent=raw>=1 ? words : words.slice(0,Math.ceil(raw*words.length)) || ' '; },
      points:()=>{
        const clip=box();
        if (!clip) return null;
        const spot={x:clip.left+Math.min(clip.width/2,60),y:clip.top+clip.height/2};
        return {grab:spot,release:spot};
      }});
  }
  if (detail.fade_in) {
    tasks.push({...base,kind:'fade',label:'Fade in',delay:wait+.1,points:()=>{
      const clip=box();
      if (!clip) return null;
      const y=clip.top+8;
      return {grab:{x:clip.left+6+frames(detail.fade_in[0]),y},release:{x:Math.max(clip.left+6,clip.left+frames(detail.fade_in[1])),y}};
    }});
  }
  if (detail.fade_out) {
    tasks.push({...base,kind:'fade',label:'Fade out',delay:wait+.2,points:()=>{
      const clip=box();
      if (!clip) return null;
      const y=clip.top+8;
      return {grab:{x:clip.right-6-frames(detail.fade_out[0]),y},release:{x:Math.min(clip.right-6,clip.right-frames(detail.fade_out[1])),y}};
    }});
  }
  if (detail.gain) {
    const [from,to]=detail.gain;
    const level=(clip,db)=>clip.top+clip.height*(.5-Math.max(-24,Math.min(24,db))/60);
    tasks.push({...base,kind:'gain',label:(to<0 ? '\u2212' : '+')+Math.abs(Math.round(to*10)/10)+' dB',delay:wait+.4,points:()=>{
      const clip=box();
      if (!clip) return null;
      const x=clip.left+Math.min(clip.width*.5,70);
      return {grab:{x,y:level(clip,from)},release:{x,y:level(clip,to)}};
    }});
  }
  if (detail.transition) {
    const kind=detail.transition[1];
    const edge=()=>{
      const clip=box();
      return clip && {x:clip.left,y:clip.top+clip.height/2};
    };
    if (!kind) {
      tasks.push({...base,kind:'change',label:'No transition',delay:wait+.1,points:()=>{
        const spot=edge();
        return spot && {grab:spot,release:spot};
      }});
    } else {
      const rows=['Dissolve','Dip to Black','Wipe'];
      const row=/wipe/i.test(kind) ? 2 : /dip|black|fade/i.test(kind) ? 1 : 0;
      let menu;
      tasks.push({...base,kind:'transition',label:rows[row],delay:wait+.1,steady:true,
        points:()=>{
          const spot=edge();
          return spot && {grab:spot,release:{x:spot.x+62,y:spot.y+20+21*row}};
        },
        position:(raw,grab,release)=>{
          const u=Math.max(0,Math.min(1,(raw-.3)/.45));
          const ease=u<.5 ? 4*u*u*u : 1-Math.pow(-2*u+2,3)/2;
          return {x:grab.x+(release.x-grab.x)*ease,y:grab.y+(release.y-grab.y)*ease};
        },
        progress:raw=>{
          if (!menu && raw<.9) {
            const spot=edge();
            if (!spot) return;
            menu=item('div','magic-menu','');
            rows.forEach(name=>menu.append(item('span','',name)));
            Object.assign(menu.style,{left:spot.x+4+'px',top:spot.y+6+'px'});
            overlay.append(menu);
          }
          if (!menu) return;
          const opening=Math.min(1,raw/.15);
          menu.style.opacity=opening;
          menu.style.transform='scale('+(.85+.15*opening)+')';
          menu.children[row].classList.toggle('picked',raw>.6);
          if (raw>=.9) { menu.remove(); menu=null; }
        },
        finish:()=>menu?.remove()});
    }
  }
  if (detail.grade) {
    const [old,now]=detail.grade.map(grade=>grade || {});
    const slot=cards.next();
    const fields=[['exposure','Exposure',2,2],['contrast','Contrast',50,0],['saturation','Saturation',100,0]];
    const signed=(value,digits)=>(value<0 ? '\u2212' : '+')+Math.abs(value).toFixed(digits);
    let card;
    const build=()=>{
      if (card) return;
      const frame=cards.frame(slot);
      card=item('div','magic-grade-card','');
      Object.assign(card.style,{left:frame.left+'px',top:frame.top+'px',width:frame.width+'px'});
      const title=item('header','','');
      title.append(item('b','','GRADE'),item('span','',step.label || clipNode(step.clip)?.querySelector('strong')?.textContent || 'Clip'));
      const look=item('p','magic-look','');
      look.append(item('span','','Look'),item('i','',old.look || 'None'));
      card.append(title,look);
      for (const [key,name,range,digits] of fields) {
        const line=item('p','magic-slider','');
        line.dataset.key=key;
        line.append(item('span','',name),item('u','',''),item('em','',signed(old[key] || 0,digits)));
        line.style.setProperty('--at',(old[key] || 0)/range);
        card.append(line);
      }
      cards.show(card);
    };
    const chain=[];
    const lookName=look=>String(look).split('/').at(-1).replace(/^lut:/,'').replace(/\.cube$/i,'');
    if ((old.look || '')!==(now.look || '')) {
      chain.push({kind:'look',label:now.look ? lookName(now.look) : 'No look',
        points:()=>{
          const frame=cards.frame(slot);
          const spot={x:frame.left+119,y:frame.top+33};
          return {grab:spot,release:spot};
        },
        begin:()=>card?.querySelector('.magic-look i').classList.add('picking'),
        finish:()=>{
          const chip=card?.querySelector('.magic-look i');
          if (!chip) return;
          chip.classList.remove('picking');
          chip.textContent=now.look ? lookName(now.look) : 'None';
        }});
    }
    fields.forEach(([key,name,range,digits],index)=>{
      const [from,to]=[old[key] || 0,now[key] || 0];
      if (from===to) return;
      const knob=(frame,value)=>frame.left+84+(frame.width-130)*(.5+Math.max(-1,Math.min(1,value/range))/2);
      chain.push({kind:'grade',label:name,
        points:()=>{
          const frame=cards.frame(slot);
          const y=frame.top+54+15*index+6;
          return {grab:{x:knob(frame,from),y},release:{x:knob(frame,to),y}};
        },
        apply:u=>{
          const line=card?.querySelector('[data-key='+key+']');
          if (!line) return;
          const value=from+(to-from)*u;
          line.style.setProperty('--at',value/range);
          line.querySelector('em').textContent=signed(value,digits);
        }});
    });
    if (!chain.length) chain.push({kind:'change',label:'Colour',points:()=>{
      const frame=cards.frame(slot);
      const spot={x:frame.left+frame.width/2,y:frame.top+48};
      return {grab:spot,release:spot};
    }});
    chain.forEach((task,index)=>{
      Object.assign(task,{clip:step.clip,steady:true,after:index ? chain[index-1] : after,same:index>0,
        delay:index ? .03 : wait+.1+Math.random()*.35,approach:index ? undefined : build});
      if (!index) { const begin=task.begin; task.begin=()=>{ build(); begin?.(); }; }
    });
    const last=chain.at(-1);
    last.cool=()=>{
      const clip=box();
      if (card) cards.hide(card,clip ? {x:clip.left+clip.width/2,y:clip.top+clip.height/2} : {x:innerWidth/2,y:innerHeight/2});
    };
    tasks.push(...chain);
  }
  return tasks;
}
// While the swarm works, the viewer flashes the clips of the cut, as Editroom does.
function flashViewer(steps) {
  const clips=project.timeline.filter(clip=>source(clip.source)?.video).sort((a,b)=>a.start-b.start);
  if (!clips.length) return ()=>{};
  const stage=byId('viewer-picture');
  const flash=item('div','magic-flash','');
  const picture=new Image();
  const pill=item('span','','');
  flash.append(picture,pill);
  flash.setAttribute('aria-hidden','true');
  flash.hidden=true;
  stage.append(flash);
  let turn=0;
  const show=()=>{
    const clip=clips[turn++%clips.length];
    const asset=source(clip.source);
    const next=new Image();
    next.onload=()=>{
      picture.src=next.src;
      pill.textContent=asset.title+'  '+time(clip.start+.6*length(clip));
      flash.hidden=false;
    };
    next.src=sourceUrl('thumb',asset.id,asset.version);
  };
  const first=setTimeout(show,1000);
  const timer=setInterval(show,1300);
  return ()=>{
    clearTimeout(first);
    clearInterval(timer);
    flash.remove();
  };
}
async function performDirectorEdits(steps) {
  const show=thinkingShow;
  thinkingShow=null;
  thinkingRun=null;
  if (!showDirectorWork || calmMotion() || !window.MagicShow) {
    show?.end();
    return replayDirectorEdits(steps);
  }
  show?.handOver();
  swarmShow?.skip();
  const editor=projectId;
  const revision=project.revision;
  zoomTimeline(null);
  timelineBody.scrollTop=0;
  // Clips the swarm will place wait out of sight; clips it will move or trim start where they were.
  const waiting=steps.filter(step=>step.before==null && step.after!=null && !step.ruler_item && !step.track_id);
  for (const step of waiting) clipNode(step.clip)?.classList.add('magic-waiting');
  const {tasks,overlay}=swarmTasks(steps);
  for (const task of tasks) if (task.kind==='trim' || task.kind==='move') task.apply(0);
  for (const task of tasks) if (task.kind==='type') task.progress(0);
  const clear=()=>{
    overlay.remove();
    for (const node of timelineBody.querySelectorAll('.magic-waiting,.magic-hot,[data-magic-width]')) {
      node.classList.remove('magic-waiting','magic-hot','magic-trim-head','magic-trim-tail','magic-moving');
      node.style.transform='';
    }
    show?.release();
  };
  // Hold on the sketch of the cut before the cursors leave the button.
  await new Promise(resolve=>setTimeout(resolve,show ? Math.min(2000,Math.max(800,show.drawnIn*1000+350)) : 0));
  if (projectId!==editor || project.revision!==revision) return clear();
  const button=byId('toggle-director').getBoundingClientRect();
  const running=MagicShow.swarm({tasks,origin:()=>({x:button.left+button.width/2,y:button.top+button.height/2})});
  swarmShow=running;
  const stopFlashes=flashViewer(steps);
  // As in Editroom, the editor is locked while the swarm works. Esc stops the show and takes the
  // edit back; the Director panel and the app menus stay usable.
  let reverted=false;
  const hurry=event=>{
    if (event.type==='keydown') {
      if (event.metaKey || event.ctrlKey || ['Shift','Meta','Control','Alt','Tab'].includes(event.key)) return;
      event.preventDefault();
      event.stopPropagation();
      if (event.key!=='Escape') return;
      reverted=true;
      running.skip();
      return;
    }
    if (event.target.closest?.('#director-panel,.top,dialog')) return;
    event.preventDefault();
    event.stopPropagation();
  };
  addEventListener('pointerdown',hurry,true);
  addEventListener('keydown',hurry,true);
  await running.done;
  removeEventListener('pointerdown',hurry,true);
  removeEventListener('keydown',hurry,true);
  if (swarmShow===running) swarmShow=null;
  stopFlashes();
  clear();
  if (reverted && projectId===editor && project.revision===revision && savedProject.canUndo) {
    undoableRun=null;
    byId('undo').click();
    status('Edit taken back.');
  }
}

async function replayDirectorEdits(steps,signal) {
  if (!showDirectorWork || signal?.aborted) return;
  directorAnimation?.abort();
  const animation=new AbortController();
  const stop=()=>animation.abort();
  directorAnimation=animation;
  signal?.addEventListener('abort',stop,{once:true});
  try { await replayEdits(steps,animation.signal,null,Boolean(signal)); }
  finally {
    signal?.removeEventListener('abort',stop);
    if (directorAnimation===animation) directorAnimation=null;
  }
}

async function replayEdits(steps,signal,agent=null,draft=Boolean(signal)) {
  const reduced=matchMedia('(prefers-reduced-motion: reduce)').matches;
  if (reduced && !agent) return;
  const groups = [];
  for (const step of steps) {
    const previous = groups.at(-1);
    if (step.batch !== undefined && step.batch === previous?.[0].batch) previous.push(step);
    else groups.push([step]);
  }
  const editor = projectId;
  for (const group of groups.slice(0,12)) {
    if (projectId !== editor || signal?.aborted) return;
    if (agent) {
      const times=group.filter(step=>!step.track_id).flatMap(step=>[step.before,step.after]).filter(at=>at!=null);
      if (times.length) {
        const first=Math.min(...times);
        const last=Math.max(...times);
        const width=timelineViewWidth();
        const left=timelineBody.scrollLeft;
        if (first*pxPerTick<left || last*pxPerTick>left+width-44) {
          const fit=(width-68)/Math.max(1,last-first);
          const scale=Math.max(minimumTimelineZoom(),Math.min(pxPerTick,fit));
          setTimeView(scale,(first+last)*scale/2-width/2);
        }
      }
      const primary=group[0];
      if (!primary.ruler_item && !primary.track_id) {
        const lane=[...timelineBody.querySelectorAll('[data-track]')].find(node=>node.dataset.track===primary.track);
        if (lane) {
          const clip=lane.getBoundingClientRect();
          const body=timelineBody.getBoundingClientRect();
          const center=clip.top+clip.height/2;
          if (center<body.top+byId('ruler').offsetHeight || center>body.bottom) {
            timelineBody.scrollTop+=center-body.top-body.height/2;
          }
        }
      }
    } else if (draft) {
      const target=group.find(step=>step.after!=null && !step.track_id);
      if (target) {
        const x=target.after*pxPerTick;
        const left=timelineBody.scrollLeft;
        if (x<left || x>left+timelineViewWidth()-40) timelineBody.scrollLeft=x-timelineViewWidth()*.45;
      }
    }
    await Promise.all(group.map((step,index) => animateEdit(step,index === 0,signal,agent,reduced,draft)));
  }
}

async function animateEdit(step,showCursor,signal,agent,reduced,draft) {
  const lanes = [...document.querySelectorAll('[data-track]')];
  const ruler=byId('ruler');
  const rulerItem=step.ruler_item;
  const header=step.track_id && timelineBody.querySelector('[data-track-header="'+step.track_id+'"]');
  const trackTarget=header || byId('timeline-body').querySelector('.ruler-label');
  if (header) header.scrollIntoView({block:'nearest',inline:'nearest'});
  const lane = rulerItem ? ruler : step.track_id ? trackTarget : lanes.find(node => node.dataset.track === step.track);
  if (!lane) return;
  const fromLane = lanes.find(node => node.dataset.track === step.from_track) || lane;
  const tile = [...document.querySelectorAll('[data-asset]')].find(node => node.dataset.asset === step.source);
  const band = step.action==='set_transition' &&
    [...lane.querySelectorAll('[data-transition]')].find(node => node.dataset.transition===step.clip);
  const mark = rulerItem==='marker' ? markerButton(step.clip) : rulerItem
    ? byId('ruler-marks').querySelector('[data-edge='+rulerItem+']') : null;
  const placeholder = header || mark || band || [...lane.querySelectorAll('[data-clip]')].find(node => node.dataset.clip === step.clip);
  const box = lane.getBoundingClientRect();
  const start = fromLane.getBoundingClientRect();
  const tileBox = (tile || (agent && step.source ? byId('media-title') : null))?.getBoundingClientRect();
  const before = step.before ?? step.after ?? 0;
  const after = step.after ?? step.before ?? 0;
  const fromX = step.before === null && tileBox ? tileBox.left + tileBox.width / 2 : start.left + before*pxPerTick;
  const fromY = step.before === null && tileBox ? tileBox.top + tileBox.height / 2 : start.top + start.height / 2;
  const toX = band ? band.getBoundingClientRect().left : box.left + after*pxPerTick;
  const toY = box.top + box.height/2;
  const cursor = agent ? agentCursor(agent) : item('span','director-cursor','➤');
  if (reduced) {
    if (showCursor) parkAgent(agent,cursor,toX,toY);
    return;
  }
  const asset=source(step.source) || (step.after===null && savedProject?.sources.find(item=>item.id===step.source));
  const ghost = item('span','director-ghost',step.label || band?.title || asset?.title || placeholder?.textContent || 'Clip');
  if (agent) ghost.style.borderColor=agentColor(agent);
  if (asset?.video) {
    ghost.classList.add('with-thumb');
    ghost.style.setProperty('--director-poster','url('+sourceUrl('thumb',asset.id,asset.version)+')');
  }
  cursor.setAttribute('aria-hidden','true');
  ghost.setAttribute('aria-hidden','true');
  document.body.append(ghost);
  if (showCursor) document.body.append(cursor);
  if (draft && placeholder && !step.track_id &&
      (step.before!==step.after || step.from_track!==step.track || step.before_width!==step.after_width)) {
    placeholder.classList.add('director-arriving');
  }
  const pixel = (frame, fallback) => step.track_id ? Math.min(140,box.width) : Math.max(8,(frame || fallback)*pxPerTick);
  const mov = cursorPosition;
  const options={duration:650,easing:'cubic-bezier(.23,.83,.28,1)'};
  const parked=agent && showCursor ? releaseAgent(agent.id) : null;
  const pointer = showCursor ? cursor.animate([
    {transform:parked ? mov(parked.x,parked.y) : mov(fromX,fromY),opacity:parked ? 1 : 0},
    {transform:mov(fromX,fromY),opacity:1,offset:parked ? .24 : .14},
    {transform:mov(toX,toY),opacity:1,offset:.82},
    {transform:mov(toX,toY),opacity:agent ? 1 : 0}
  ],options) : null;
  const movements = ghost.animate([
    {transform:mov(fromX,fromY-11),width:pixel(step.before_width,ticks(rulerItem ? 2 : 0.625))+'px',opacity:0},
    {transform:mov(fromX,fromY-11),width:pixel(step.before_width,ticks(rulerItem ? 2 : 0.625))+'px',opacity:.82,offset:.2},
    {transform:mov(toX,toY-11),width:pixel(step.after_width,ticks(rulerItem ? 2 : 0.625))+'px',opacity:step.after === null ? 0 : .78,offset:.83},
    {transform:mov(toX,toY-11),width:pixel(step.after_width,ticks(rulerItem ? 2 : 0.625))+'px',opacity:0}
  ],options);
  const pulse=placeholder?.animate([{filter:'brightness(.8)'},{filter:'brightness(1.5)'},{filter:'brightness(1)'}],options);
  const cancel=()=>{movements.cancel();pointer?.cancel();pulse?.cancel();};
  signal?.addEventListener('abort',cancel,{once:true});
  try { await movements.finished.catch(() => {}); }
  finally {
    signal?.removeEventListener('abort',cancel);
    placeholder?.classList.remove('director-arriving');
    cursor.remove();
    ghost.remove();
    if (agent && showCursor && !signal?.aborted) parkAgent(agent,cursor,toX,toY);
  }
}

const promptField = byId('prompt');
const mentionList = byId('mentions');
let mentionChoices = [];
let activeMention = -1;
let mentionStart = 0;

const mentionKinds = {
  video:'video',videos:'video',audio:'audio',sound:'audio',music:'audio',
  photo:'image',photos:'image',image:'image',images:'image',text:'text',texts:'text',
  title:'text',titles:'text',solid:'solid',solids:'solid'
};
const mentionKindLabels = {video:'Video',audio:'Audio',image:'Photo',text:'Text',solid:'Solid'};
const mentionTime = at => {
  const seconds = Math.max(0,at) / TICKS_PER_SECOND;
  return Math.floor(seconds / 60) + ':' + (seconds % 60).toFixed(1).padStart(4,'0');
};

function closeMentions() {
  mentionList.hidden = true;
  mentionChoices = [];
  activeMention = -1;
  promptField.removeAttribute('aria-activedescendant');
}

function referenceChoices(query) {
  const targets = project.director_mentions || [];
  const groups = Object.groupBy(targets,target=>target.kind);
  const row = target => {
    const kind=target.kind;
    const name=(kind === 'media' ? '' : kind+':')+target.name;
    const literal = /[\s"\\]/u.test(target.name) ? JSON.stringify(target.name) : target.name;
    const detail = kind === 'marker' ? 'Marker · '+mentionTime(target.at)
      : kind === 'clip' ? target.track+' · '+mentionTime(target.at)+' · '+mentionKindLabels[target.media_kind]
      : target.media_kind === 'image' ? 'Photo' : mentionTime(target.at)+' · '+mentionKindLabels[target.media_kind];
    return {name,text:kind+':'+literal,detail,color:target.color};
  };
  const byName = (entries,filter) => {
    const first = entries.filter(target=>target.name.toLowerCase().startsWith(filter));
    const rest = filter && !filter.includes(' ') ? entries.filter(target=>{
      const name=target.name.toLowerCase();
      return !name.startsWith(filter) && name.includes(filter);
    }) : [];
    return [...first,...rest].map(row);
  };
  const prefix = /^(clip|media|marker):/u.exec(query)?.[1];
  if (prefix) {
    const filter = query.slice(prefix.length+1).replace(/^"/u,'');
    const entries = groups[prefix] || [];
    const kind = Object.hasOwn(mentionKinds,filter) && mentionKinds[filter];
    if (kind && prefix !== 'marker' && (prefix === 'clip' || !['text','solid'].includes(kind))) {
      return entries.filter(target=>target.media_kind===kind).map(row);
    }
    return byName(entries,filter);
  }
  const filter = query.replace(/^"/u,'');
  const selectedClips = project.timeline.filter(clip=>selection.has(clip.id));
  const pictures = new Set(selectedClips.filter(clip=>clip.kind!=='audio').map(clip=>clip.link).filter(Boolean));
  const count = selectedClips.filter(clip=>clip.kind!=='audio' || !pictures.has(clip.link)).length;
  const pointers = [
    {name:'selected',text:'selected',detail:count ? count+(count===1 ? ' clip' : ' clips') : 'Select clips first',disabled:!count},
    {name:'playhead',text:'playhead',detail:tc(frameAt)}
  ];
  if (project.play_range) pointers.push({name:'in-out',text:'in-out',detail:project.play_range.map(mentionTime).join(' to ')});
  for (const [kind,description] of [['media','in Media'],['clip','on the timeline'],['marker','on the timeline']]) {
    const total=groups[kind]?.length || 0;
    if (total) pointers.push({name:kind+':',text:kind+':',detail:total+' '+description+' ›',opens:true});
  }
  const shown=pointers.filter(option=>option.name.startsWith(filter));
  return filter ? [...shown,...byName(groups.marker || [],filter),...byName(groups.media || [],filter)] : shown;
}

function selectMention(index) {
  activeMention=index;
  const active = mentionList.children[index];
  for (const child of mentionList.children) child.setAttribute('aria-selected',String(child === active));
  if (active) {
    promptField.setAttribute('aria-activedescendant',active.id);
    active.scrollIntoView({block:'nearest'});
  } else {
    promptField.removeAttribute('aria-activedescendant');
  }
}

function insertMention(choice) {
  promptField.setRangeText('@'+choice.text+(choice.opens ? '' : ' '),mentionStart,promptField.selectionEnd,'end');
  savePromptDraft(promptField.value);
  if (choice.opens) updateMentions();
  else closeMentions();
  promptField.focus();
}

function updateMentions() {
  const text = promptField.value.slice(0,promptField.selectionStart);
  const typed = /(?:^|[^\p{L}\p{N}_])@((?:(?:clip|marker|media):)?(?:"[^"\n]*|[^\s@"]*))$/iu.exec(text);
  if (!typed || !project) return closeMentions();
  mentionStart=text.length-typed[1].length-1;
  mentionChoices=referenceChoices(typed[1].toLowerCase());
  if (!mentionChoices.length) return closeMentions();
  mentionList.replaceChildren(...mentionChoices.map((choice,index)=>{
    const option=item('button','button mention-option','');
    option.type='button';
    option.id='director-mention-'+index;
    option.tabIndex=-1;
    option.disabled=Boolean(choice.disabled);
    option.setAttribute('role','option');
    const name=item('span','mention-name','@'+choice.name);
    if (choice.color) {
      const dot=item('span','mention-marker','');
      dot.style.backgroundColor=choice.color;
      dot.setAttribute('aria-hidden','true');
      name.prepend(dot);
    }
    option.append(name,item('small','mention-detail',choice.detail));
    option.onmousedown=event=>event.preventDefault();
    option.onclick=()=>insertMention(choice);
    return option;
  }));
  mentionList.hidden=false;
  mentionList.scrollTop=0;
  selectMention(mentionChoices.findIndex(choice=>!choice.disabled));
}

promptField.addEventListener('input',()=>{
  savePromptDraft(promptField.value);
  updateMentions();
});
promptField.addEventListener('click',updateMentions);
promptField.addEventListener('blur',closeMentions);
promptField.addEventListener('focus',()=>{
  if (!quick && selection.size && ['', '@selected'].includes(promptField.value.trim())) {
    promptField.value='@selected ';
    promptField.setSelectionRange(promptField.value.length,promptField.value.length);
  } else if (!quick && !selection.size && promptField.value.trim()==='@selected') {
    promptField.value='';
  }
});

function previousPrompts() {
  const entries=[...(project?.prompt_history || [])];
  const seen=new Set(entries.map(entry=>entry.text));
  for (const message of (project?.messages || []).toReversed()) {
    if (message.from!=='you' || seen.has(message.text)) continue;
    entries.push({text:message.text});
    seen.add(message.text);
  }
  return entries;
}

function fillDirectorPrompt(text) {
  closeMentions();
  promptField.value=text;
  savePromptDraft(promptField.value);
  promptField.focus();
  promptField.setSelectionRange(promptField.value.length,promptField.value.length);
}

byId('prompt-history').onclick=event=>{
  const tabs={chatgpt:'ChatGPT',claude:'Claude',local:'Local',video:'Quick Clips'};
  const commands=previousPrompts().slice(0,20).map(({text,tab})=>{
    const preview=text.replace(/\s+/gu,' ').trim();
    const label=preview.length>72 ? preview.slice(0,71)+'…' : preview;
    return [tabs[tab] ? label+' · '+tabs[tab] : label,()=>fillDirectorPrompt(text)];
  });
  if (!commands.length) commands.push(['Prompts you send show up here.',()=>{},true]);
  openTimelineMenu(event,commands,'Prompt history',event.currentTarget);
};

const directorIdeas={
  improve:[
    ['Tighten it','Make this shorter so people keep watching: open on the strongest moment, cut the slow starts and ends of shots, and drop weak or repeated shots.'],
    ['Cut to the beat','Recut this to the beat of the music: cuts on beats, the biggest moments on bar starts, and end on a bar.'],
    ['Smooth it out','Smooth this out: fix jumpy cuts, add a dissolve only where the scene changes, even out the sound, and fade in and out.'],
    ['One look','Give it one consistent look: the same warm film grade on every shot, matched in brightness so it feels like one film.'],
  ],
  create:[
    ['Story','Make the best story you can from this footage: a clear beginning, middle and end, the strongest moments where they land hardest, and shots left to breathe where it matters.'],
    ['Highlight reel','Make a 30 to 60 second highlight reel of the best moments, cut to the beat of the music, opening on the strongest shot and ending on a bar.'],
    ['Timelapse','Make a timelapse over the music: speed the footage up, cut out the dead space where nothing changes, keep the moments where something happens, and end on the strongest shot.'],
    ['Vlog','Make an engaging vlog, up to 8 minutes: hook in the first few seconds, keep the parts where someone is talking or something happens, cut pauses, retakes and repeats, and add short titles to mark each part.'],
    ['Vertical reel','Make a fast, punchy 15 to 30 second vertical video for Reels and TikTok: 1080 by 1920, a hook in the first second, quick cuts on the beat.'],
    ['Find music','Find music that suits this video and add it to the timeline. Make sure the music is royalty free and does not require attribution.'],
    ['Music and levels','Pick the best music in my Media for this edit, cut so the music and pictures land together, keep voices clear over it, and fade in and out.'],
  ],
};
const localDirectorIdeas={
  improve:[
    ['Tighten it','Make it shorter and faster.'],
    ['Cut to the beat','Cut it to the beat of the music.'],
    ['One look','Give every shot the Film print look.'],
    ['Clean it up','Take out repeats and weak shots, and trim long shots.'],
  ],
  create:[
    ['Quick cut','Make a new quick cut of the best moments, to the beat.'],
    ['30-second highlight','Make a fast 30 second highlight cut to the beat.'],
    ['Vertical reel','Make a 20 second vertical video, fast.'],
    ['Timelapse','Speed every shot up 8x, over the music.'],
    ['Slow and calm','Make a calm 60 second cut.'],
    ['Even out levels','Make all audio clips the same output level.'],
  ],
};

byId('prompt-ideas').onclick=event=>{
  const ideas=state.provider==='local' && state.builtin_local ? localDirectorIdeas : directorIdeas;
  const available=project.timeline.length ? [...ideas.improve,...ideas.create] : ideas.create;
  const commands=available.map(([label,text])=>[label,()=>fillDirectorPrompt(text)]);
  openTimelineMenu(event,commands,'Editing ideas',event.currentTarget);
};

promptField.addEventListener('keydown',event=>{
  if (event.isComposing) return;
  if (!mentionList.hidden && !event.metaKey && !event.ctrlKey && !event.shiftKey && !event.altKey) {
    if (['ArrowUp','ArrowDown','Enter','Tab','Escape'].includes(event.key)) {
      event.preventDefault();
      event.stopPropagation();
      if (event.key==='Escape') closeMentions();
      else if (['Enter','Tab'].includes(event.key)) {
        if (activeMention>=0) insertMention(mentionChoices[activeMention]);
        else closeMentions();
      } else {
        const usable=mentionChoices.flatMap((choice,index)=>choice.disabled ? [] : [index]);
        if (usable.length) {
          const next=usable.indexOf(activeMention)+(event.key==='ArrowDown' ? 1 : -1);
          selectMention(usable[(next+usable.length)%usable.length]);
        }
      }
      return;
    }
  }
  if (event.key==='Enter' && (!event.shiftKey || event.metaKey || event.ctrlKey)) {
    event.preventDefault();
    byId('prompt-form').requestSubmit();
  } else if (['ArrowUp','ArrowDown'].includes(event.key) && !event.shiftKey && !event.metaKey && !event.ctrlKey && !event.altKey) {
    const previous=previousPrompts().map(entry=>entry.text);
    const current=promptField.value.trim();
    const index=previous.indexOf(current);
    if (!previous.length || (index<0 && current && current!=='@selected') || (index<0 && event.key==='ArrowDown')) return;
    event.preventDefault();
    const next=event.key==='ArrowUp' ? Math.min(index+1,previous.length-1) : index-1;
    promptField.value=previous[next] || '';
    savePromptDraft(promptField.value);
    promptField.setSelectionRange(promptField.value.length,promptField.value.length);
  }
});

const clipMenu = byId('clip-menu');
let timelineMenuAnchor;
let timelineMenu;
const transitionNames={dissolve:'Dissolve',dipToBlack:'Dip to Black',wipe:'Wipe'};
function cutForClip(clip,side) {
  return (project.cuts || []).find(cut => cut[side==='in'?'incoming':'previous']===clip?.id);
}
function renderTransitionBands(lane,track) {
  for (const cut of project.cuts || []) {
    const clip=project.timeline.find(clip=>clip.id===cut.incoming);
    if (!cut.span || !clip || clipTrack(clip)!==track.name) continue;
    const button=item('button','transition-band','');
    button.type='button';
    button.dataset.transition=cut.incoming;
    styleTransitionBand(button,cut,track.hidden);
    button.setAttribute('aria-label',button.title);
    button.onclick=()=>{
      selectClip(clip.id,{seek:false});
      byId('transition-controls').open=true;
      seekTimeline((cut.span.start+cut.span.end)/2);
    };
    lane.append(button);
  }
}
function styleTransitionBand(element,cut,hidden) {
  element.style.left=(cut.span.start*pxPerTick)+'px';
  element.style.width=Math.max(4,(cut.span.end-cut.span.start)*pxPerTick)+'px';
  element.style.opacity=hidden ? '.5' : '';
  element.title=transitionNames[cut.kind]+' · '+time(cut.span.end-cut.span.start)+'s';
}
function setCutTransition(cut,kind,duration) {
  return savePicture('set_transition',{clip_id:cut.incoming,kind,duration});
}
function dissolveCutAtSelection() {
  let nearest;
  const at=playheadTicks();
  const cuts=new Map(project.cuts.map(cut=>[cut.incoming,cut]));
  for (const clip of project.timeline) {
    const cut=cuts.get(clip.id);
    if (!cut || trackFor(clip)?.locked || (!selection.has(cut.previous) && !selection.has(cut.incoming))) continue;
    if (!nearest || Math.abs(cut.at-at)<Math.abs(nearest.at-at)) nearest=cut;
  }
  return nearest && !nearest.kind ? nearest : null;
}
function addDissolve() {
  const cut=dissolveCutAtSelection();
  if (cut) return setCutTransition(cut,'dissolve',.5);
}
function transitionCommands(cut) {
  const clip=project.timeline.find(clip=>clip.id===cut.incoming);
  const locked = trackFor(clip)?.locked;
  return [['None',()=>setCutTransition(cut,'none'),locked,!cut.kind],
    ...Object.entries(transitionNames).map(([kind,label])=>[label,()=>setCutTransition(cut,kind),locked,cut.kind===kind])
  ];
}
const cutFields={
  in:{kind:byId('clip-transition'),length:byId('transition-duration')},
  out:{kind:byId('clip-transition-out'),length:byId('transition-out-duration')}
};
function renderTransitionControls(clip) {
  if (!clip) {
    byId('transition-controls').hidden=true;
    return;
  }
  const cuts={in:cutForClip(clip,'in'),out:cutForClip(clip,'out')};
  byId('transition-controls').hidden=!cuts.in&&!cuts.out;
  for (const [side,cut] of Object.entries(cuts)) {
    const fields=cutFields[side];
    byId('transition-'+side).hidden=!cut;
    if (!cut) continue;
    fields.kind.value=cut.kind||'none';
    fields.length.value=cut.requested ? cut.requested/TICKS_PER_SECOND : .5;
    byId('transition-'+side+'-length').hidden=!cut.kind;
    const info=byId('transition-'+side+'-info');
    const span=cut.span;
    const actual=span ? span.end-span.start : 0;
    info.hidden=!cut.kind || actual>=cut.requested-ticks(.01);
    if (!info.hidden) {
      info.textContent=actual ? 'Shortened to '+(actual/TICKS_PER_SECOND).toFixed(2)+'s by the available footage.'
        : 'Both clips run to the edge of their footage at this cut. Dip to Black works here.';
    }
  }
}
for (const [side,fields] of Object.entries(cutFields)) {
  fields.kind.onchange=()=>{
    const cut=cutForClip(onlySelectedPicture(),side);
    if (cut) setCutTransition(cut,fields.kind.value);
  };
  fields.length.onchange=()=>{
    const clip=onlySelectedPicture();
    const cut=cutForClip(clip,side);
    const value=Number(fields.length.value);
    if (cut?.kind && value>=.1 && value<=5) setCutTransition(cut,cut.kind,value);
    else if (cut) renderTransitionControls(clip);
  };
}

function detachSelection() {
  if (linkedSelection(selection).some(clip=>clip.link)) return mcp('detach_audio',selectionArgs());
}
function canFreeze(clip) {
  const at=playheadTicks();
  return clip.kind==='video' && !clip.effects.freeze && !trackFor(clip)?.locked &&
      at>=timelineStart(clip.id) && at<timelineEnd(clip);
}
async function freezeSelected(clip = project.timeline.find(clip=>selection.has(clip.id) && canFreeze(clip))) {
  if (!clip || !canFreeze(clip)) return;
  const at=playheadTicks()/TICKS_PER_SECOND;
  const editor=projectId;
  const changed=await mcp('freeze_frame',{clip_id:clip.id,at},{editor});
  const held=changed?.find(clip=>clip.freeze_frame && Math.abs(clip.start-at)<frameStep()/TICKS_PER_SECOND);
  if (held && projectId===editor) selectClip(held.id,{seek:false});
}
function canReverse(clip) {
  return ['video','audio'].includes(clip.kind) && !clip.effects.freeze && !trackFor(clip)?.locked;
}
function reverseSelected(clip = project.timeline.find(clip=>selection.has(clip.id) && canReverse(clip))) {
  if (clip && canReverse(clip)) return mcp('reverse_clip',{clip_id:clip.id});
}

function openClipCommands(event) {
  const unlocked=linkedSelection(selection);
  const at=onFrame(playheadTicks());
  const half=Math.ceil(frameStep()/2);
  const under=project.timeline.filter(clip=>!trackFor(clip)?.locked &&
    timelineStart(clip.id)<at && timelineEnd(clip)>at);
  const pickedUnder=new Set(under.filter(clip=>selection.has(clip.id)).map(clip=>clip.id));
  const trimTracks=new Set(linkedSelection(pickedUnder).map(clipTrack));
  const edges=trimTracks.size ? under.filter(clip=>trimTracks.has(clipTrack(clip))) : under;
  const matchable=project.timeline.find(clip=>selection.has(clip.id) && canMatchSequence(clip));
  const pictures=editablePictures();
  const captionTargets=captionedTargets(selection);
  const canCaption=captionTargets.length || (!selection.size && canAddDefaultCaptions());
  const canDelete=unlocked.length || libraryPickCount() || captionSelection.size;
  const canSplit=under.some(clip=>at-timelineStart(clip.id)>=half && timelineEnd(clip)-at>=half);
  openEditorCommands(event,'Clip',[
    ['Split at Playhead','split',!canSplit],
    ['Add Dissolve','addDissolve',!dissolveCutAtSelection()],
    ['Even Out Audio Levels',()=>byId('even-levels').click(),byId('even-levels').disabled],
    ['Detach Audio','detachAudio',!unlocked.some(clip=>clip.link)],
    ['Duplicate','duplicate',!unlocked.length],
    ['Freeze Frame','freezeFrame',!unlocked.some(clip=>selection.has(clip.id) && canFreeze(clip))],
    ['Reverse','reverse',!unlocked.some(clip=>selection.has(clip.id) && canReverse(clip))],
    ['Copy Grade','copyGrade',pictures.length!==1],
    ['Paste Grade','pasteGrade',!pictures.length || copiedGrade===undefined],
    ['Match Sequence to Clip',()=>mcp('match_sequence_to_clip',{clip_id:matchable.id}),!matchable],
    ['Delete','delete',!canDelete],
    ['Ripple Delete','rippleDelete',!canDelete],
    ['Ripple Trim Start to Playhead','rippleTrimStart',!edges.length || edges.some(clip=>at-timelineStart(clip.id)<half)],
    ['Ripple Trim End to Playhead','rippleTrimEnd',!edges.length || edges.some(clip=>timelineEnd(clip)-at<half)],
    ['Close All Gaps',()=>mcp('close_all_gaps',selectionArgs())],
    ['Add Text','addText'],
    ['Add Solid',()=>addGraphic('add_solid')],
    [captionTargets.some(clip=>clip.captioned) ? 'Remove Captions' : 'Add Captions',changeClipCaptions,!canCaption],
    ['Add Video Track',()=>mcp('add_track',{kind:'video'})],
    ['Add Audio Track',()=>mcp('add_track',{kind:'audio'})],
  ]);
}
byId('clip-commands').onclick=openClipCommands;

function openEditorCommands(event,label,commands) {
  const anchor=event.currentTarget;
  if (timelineMenuAnchor===anchor) {
    closeTimelineMenu(true);
    return;
  }
  const menu=commands.map(([title,command,disabled])=>{
    const id=typeof command==='string' ? command : null;
    const shortcut=StudioShortcuts.definitions.find(item=>item.id===id);
    return [title,id ? shortcutActions[id] : command,disabled,undefined,shortcut];
  });
  openTimelineMenu(event,menu,label,anchor);
}

function showClipMenu(event,clip) {
  event.preventDefault();
  selectClip(clip.id,{keep:true,seek:false});
  const captionTargets = captionedTargets(selection);
  const unlocked = linkedSelection(selection);
  const canEdit = !trackFor(clip)?.locked;
  const at=onFrame(playheadTicks());
  const half=Math.ceil(frameStep()/2);
  const canSplit=canEdit && at-timelineStart(clip.id)>=half && timelineEnd(clip)-at>=half;
  const download=sourceDownloadUrl(source(clip.source));
  const commands = [
    ['Cut',() => copySelection(true),!unlocked.length],
    ['Copy',() => copySelection(),!unlocked.length],
    ['Paste at playhead',pasteSelection,clipClipboard?.project !== projectId],
    ['Duplicate',duplicateSelection,!unlocked.length],
    ['Split at playhead',splitSelection,!canSplit],
    ['Detach audio',detachSelection,!unlocked.some(item=>item.link)],
    ['Freeze frame at playhead',()=>freezeSelected(clip),!canFreeze(clip)],
    [clip.effects.reverse ? 'Play Forwards' : 'Reverse',()=>reverseSelected(clip),!canReverse(clip)],
    ['Even out levels',() => mcp('even_out_audio_levels',selectionArgs()),!unlocked.some(item => item.kind === 'audio')],
    [captionTargets.some(clip => clip.captioned) ? 'Remove captions' : 'Add captions',changeClipCaptions,!captionTargets.length],
    ['Remove',() => mcp('delete_clips',selectionArgs()),!unlocked.length],
    ['Ripple delete',() => mcp('delete_clips',{...selectionArgs(),ripple:true}),!unlocked.length],
  ];
  if (!clip.effects.freeze && ['video','audio'].includes(clip.kind)) {
    const presets=[25,50,100,200,400,800,1600,3000].map(percent=>[
      percent+'%',()=>changeClipSpeed(percent),false,Math.abs(clip.effects.speed*100-percent)<.5,
    ]);
    commands.push(['Speed',presets,!canEdit]);
  }
  if (unlocked.some(c => c.effects.fade_in_frames || c.effects.fade_out_frames || c.fade_in)) {
    commands.push(['Remove Fades',()=>savePicture('set_audio',{...selectionArgs(),fade_in:0,fade_out:0})]);
  }
  if (canMatchSequence(clip)) commands.push(['Match Sequence to Clip',()=>mcp('match_sequence_to_clip',{clip_id:clip.id})]);
  if (download) commands.push(['Download Source',()=>triggerBrowserDownload(download)]);
  if (clip.kind!=='audio') {
    const pictures=editablePictures();
    const paste=pictures.length>1 ? 'Paste Grade to '+pictures.length+' Clips' : 'Paste Grade';
    commands.push(['Copy Grade',copySelectedGrade,pictures.length!==1],
      [paste,pasteSelectedGrade,!pictures.length || copiedGrade===undefined]);
  }
  for (const side of ['in','out']) {
    const cut=cutForClip(clip,side);
    if (cut) commands.push([side==='in'?'Transition In':'Transition Out',transitionCommands(cut),!canEdit]);
  }
  openTimelineMenu(event,commands,'Clip actions');
}
function closeTimelineMenu(returnFocus=false) {
  const anchor=timelineMenuAnchor;
  timelineMenuAnchor=null;
  timelineMenu=null;
  anchor?.setAttribute('aria-expanded','false');
  clipMenu.hidden=true;
  if (returnFocus) (anchor || timelineBody).focus({preventScroll:true});
}
function openTimelineMenu(event,commands,label,anchor) {
  event.preventDefault();
  closeTimelineMenu();
  timelineMenuAnchor=anchor;
  anchor?.setAttribute('aria-expanded','true');
  const origin=event.currentTarget?.getBoundingClientRect();
  const point={x:event.clientX || origin?.left || 8,y:event.clientY || origin?.bottom || 8};
  paintTimelineMenu({label,commands,point});
}
function returnToParentMenu() {
  if (timelineMenu?.parent) {
    paintTimelineMenu(timelineMenu.parent,timelineMenu.parentIndex);
  }
}
function paintTimelineMenu(menu,focusIndex) {
  timelineMenu=menu;
  clipMenu.setAttribute('aria-label',menu.label);
  const commands=menu.parent ? [['Back',returnToParentMenu],...menu.commands] : menu.commands;
  const buttons=commands.map(([label,action,disabled,checked,shortcut],index) => {
    const button = item('button','button',label);
    button.type = 'button';
    button.setAttribute('role',checked===undefined?'menuitem':'menuitemradio');
    button.setAttribute('aria-label',label);
    if (checked!==undefined) button.setAttribute('aria-checked',String(checked));
    const submenu=Array.isArray(action);
    if (submenu) button.setAttribute('aria-haspopup','menu');
    const combos=shortcut ? shortcutPreferences.combos(shortcut) : [];
    if (combos.length) {
      button.setAttribute('aria-keyshortcuts',combos.map(StudioShortcuts.keyboardShortcut).join(' '));
      const hint=item('kbd','',StudioShortcuts.display(combos[0]));
      hint.setAttribute('aria-hidden','true');
      button.append(hint);
    }
    button.disabled = Boolean(disabled);
    button.onclick = () => {
      if (submenu) {
        paintTimelineMenu({label,commands:action,point:menu.point,parent:menu,parentIndex:index});
        return;
      }
      if (menu.parent && index===0) returnToParentMenu();
      else {
        closeTimelineMenu(true);
        action();
      }
    };
    return button;
  });
  clipMenu.replaceChildren(...buttons);
  clipMenu.style.removeProperty('max-height');
  clipMenu.hidden = false;
  clipMenu.scrollTop=0;
  const anchor=timelineMenuAnchor?.getBoundingClientRect();
  const x=anchor?.left ?? menu.point.x;
  let y=menu.point.y;
  if (anchor) {
    const below=innerHeight-anchor.bottom-12;
    const above=anchor.top-12;
    const menuHeight=clipMenu.offsetHeight;
    const opensAbove=menuHeight>below && above>below;
    clipMenu.style.maxHeight=Math.max(35,Math.min(menuHeight,opensAbove ? above : below))+'px';
    y=opensAbove ? anchor.top-5-clipMenu.offsetHeight : anchor.bottom+5;
  }
  clipMenu.style.left=Math.max(4,Math.min(x,innerWidth-clipMenu.offsetWidth-9))+'px';
  clipMenu.style.top=Math.max(4,Math.min(y,innerHeight-clipMenu.offsetHeight-9))+'px';
  const first=buttons.slice(menu.parent ? 1 : 0).find(button=>!button.disabled);
  (buttons[focusIndex] || first || buttons.find(button=>!button.disabled))?.focus();
}
for (const id of ['clip-commands','mark-commands','marks-menu','prompt-history','prompt-ideas']) byId(id).onkeydown=event=>{
  if (event.key!=='ArrowDown' || event.metaKey || event.ctrlKey || event.altKey || event.shiftKey) return;
  event.preventDefault();
  event.stopPropagation();
  event.currentTarget.click();
};
document.addEventListener('pointerdown',event => {
  if (!clipMenu.contains(event.target) && !timelineMenuAnchor?.contains(event.target)) closeTimelineMenu();
});
window.addEventListener('resize',()=>closeTimelineMenu());
clipMenu.addEventListener('keydown',event=>{
  if (event.key==='Escape') {
    if (timelineMenu?.parent) returnToParentMenu();
    else closeTimelineMenu(true);
  } else if (event.key==='ArrowLeft' && timelineMenu?.parent) {
    returnToParentMenu();
  } else if (event.key==='ArrowRight' && document.activeElement?.getAttribute('aria-haspopup')==='menu') {
    document.activeElement.click();
  } else if (event.key==='Tab') {
    closeTimelineMenu(Boolean(timelineMenuAnchor));
    return;
  } else if (['ArrowUp','ArrowDown','Home','End'].includes(event.key)) {
    const buttons=[...clipMenu.querySelectorAll('button:not(:disabled)')];
    const current=buttons.indexOf(document.activeElement);
    const next=event.key==='Home' ? 0 : event.key==='End' ? buttons.length-1
      : (current+(event.key==='ArrowUp' ? -1 : 1)+buttons.length)%buttons.length;
    buttons[next]?.focus();
  } else return;
  event.preventDefault();event.stopPropagation();
});
async function addGraphic(tool,fields={}) {
  const editor=projectId;
  const result=await mcp(tool,{start:playheadTicks()/TICKS_PER_SECOND,duration:5,first_free:true,...fields},{editor});
  if (result && projectId===editor) selectClip(result.added.id,{seek:false});
  return result;
}
for (const {button,type,label} of graphicDrags) {
  const control = byId(button);
  control.ondragstart = event => {
    if (!project || directorOwnsCut() || presentedFrame) {
      event.preventDefault();
      return;
    }
    clearLibrarySkim();
    event.dataTransfer.setData(type,label);
    event.dataTransfer.effectAllowed = 'copy';
  };
  control.ondragend = clearMediaDropOverlay;
}
byId('new-title').onclick = () => {byId('title-text').value='';byId('title-dialog').showModal();};
byId('title-form').onsubmit = async event => {
  event.preventDefault();
  const text = byId('title-text').value.trim();
  if (text && await addGraphic('add_title',{text,duration:Number(byId('title-length').value)})) byId('title-dialog').close();
};
byId('new-solid').onclick = () => addGraphic('add_solid');

function resetLayers() {
  for (const player of [...pictureLayers.values(),...audioLayers.values()]) {
    player.pause();player.removeAttribute('src');
    playerSignals.get(player)?.signal.disconnect();
  }
  for (const [name,bus] of trackBuses) {
    if (name === 'soundtrack') continue;
    bus.gain.disconnect();
    bus.split.disconnect();
    trackBuses.delete(name);
  }
  pictureLayers.clear();
  audioLayers.clear();
}
function pauseLayerAudio() {
  for (const player of audioLayers.values()) player.pause();
}
function sourcePosition(clip, elapsed, window) {
  const clock = window?.points || clip.source_clock;
  const {start,end} = window || clip;
  let speed = clip.effects.speed || 1;
  let offset = elapsed * speed;
  if (clock?.length > 1) {
    let hi = clock.findIndex(point => point[1] > elapsed);
    if (hi < 1) hi = hi < 0 ? clock.length - 1 : 1;
    const [from, to] = [clock[hi - 1],clock[hi]];
    speed = (to[0] - from[0]) / Math.max(1,to[1] - from[1]);
    offset = from[0] + (elapsed - from[1]) * speed;
  }
  const time = clip.effects.freeze ? start : clip.effects.reverse ? end - 1 - offset : start + offset;
  return {offset:Math.min(end-1,Math.max(start,time)),speed};
}
function timelineVideo(clip,at,playing,layers=pictureLayers,redraw=drawComposed) {
  let element = layers.get(clip.id);
  if (!element) {
    element = document.createElement('video');
    element.muted=true;element.playsInline=true;element.preload='auto';element.src=mediaUrl(clip.source);
    element.addEventListener('loadeddata',redraw,{once:true});
    element.addEventListener('seeked',redraw);
    layers.set(clip.id,element);
  }
  if (element.readyState>=1) {
    const elapsed=at-timelineStart(clip.id)+(clip.transition_plan?.lead_in || 0);
    const {offset,speed}=sourcePosition(clip,elapsed,clip.transition_source);
    const desired = offset / TICKS_PER_SECOND;
    const tolerance = playing && !clip.effects?.reverse ? 0.075 : frameStep() / TICKS_PER_SECOND / 8;
    if (Math.abs(element.currentTime - desired) > tolerance && !element.seeking) {
      element.currentTime = desired;
    }
    if (playing && !clip.effects?.freeze && !clip.effects?.reverse) {
      element.playbackRate=Math.max(.25,Math.min(16,speed));
      if (element.paused) element.play().catch(()=>{});
    } else if (!element.paused) element.pause();
  }
  return element;
}
function pictureStart(clip) {return timelineStart(clip.id)-(clip.transition_plan?.lead_in || 0);}
function pictureEnd(clip) {return timelineEnd(clip)+(clip.transition_plan?.tail_out || 0);}
function transitionProgress(edge,at) {
  return Math.max(0,Math.min(1,(at-edge.start)/Math.max(1,edge.end-edge.start)));
}

const editorSurface={canvas:compositeCanvas,paint,maxWidth:1450,maxHeight:900};

function drawComposed() {
  if (!project || sourcePreview) {compositeCanvas.hidden=true;return;}
  const total=clipTotal();
  if (!total) {compositeCanvas.hidden=true;return;}
  compositeCanvas.hidden=false;
  byId('empty-preview').hidden=true;
  drawSequence(editorSurface,pictureLayers,drawComposed,Math.min(skimFrame ?? frameAt,total-1),playingCut);
}

function drawSequence(surface,layers,redraw,at,playing,cropSettings=null,captionOutput=null) {
  const seq=project.sequence;
  const {canvas,paint,maxWidth,maxHeight}=surface;
  const visible=project.timeline.filter(clip => clip.kind!=='audio' &&
    !source(clip.source)?.missing_file &&
    at>=pictureStart(clip) && at<pictureEnd(clip) &&
    !seq.tracks.find(track=>track.name===clipTrack(clip))?.hidden)
    .sort((a,b)=>clipTrack(a).localeCompare(clipTrack(b),undefined,{numeric:true}) || timelineStart(a.id)-timelineStart(b.id));
  const caption = seq.captions?.hidden ? null : captions.findLast(block => block.start<=at && at<block.end);
  const ratio=Math.min(1,maxWidth/seq.width,maxHeight/seq.height);
  const width=Math.max(1,Math.round(seq.width*ratio)),height=Math.max(1,Math.round(seq.height*ratio));
  if(canvas.width!==width) canvas.width=width;
  if(canvas.height!==height) canvas.height=height;
  paint.fillStyle='#000000';paint.fillRect(0,0,width,height);
  const used = new Set();
  for(const clip of visible) {
    const fx=pictureEffects(clip);
    const rel=at-timelineStart(clip.id);
    const tf=clip.kind==='text' ? fx.transform||{} : transformAt(fx.transform,rel/TICKS_PER_SECOND);
    const grade=gradeForPicture(clip,fx.grade);
    paint.save();
    const size=Number(tf.scale)||1;
    const inFrames=clip.fade_in?Math.max(ticks(1 / 3),fx.fade_in_frames||0):fx.fade_in_frames||0;
    paint.globalAlpha=clipFade(clip,rel,inFrames);
    const plan=clip.transition_plan;
    if (plan?.conceal) paint.globalAlpha *= 1-transitionProgress(plan.conceal,at);
    if (plan?.reveal) {
      const p=transitionProgress(plan.reveal,at);
      if (plan.reveal.kind==='wipe') {
        if (p<1) {
          const wipe=p*p*(3-2*p);
          const x=clip.kind==='text' ? 0 : (1-size)*width/2+tf.x*width;
          const y=clip.kind==='text' ? 0 : (1-size)*height/2+tf.y*height;
          paint.beginPath();
          paint.rect(x,y,width*size*wipe,height*size);
          paint.clip();
        }
      } else paint.globalAlpha*=p;
    }
    if(clip.kind==='text') {
      drawTitle(fx,seq,width,height,gradePreview.swatch(titleColor(fx),grade),paint);
    } else if(clip.kind==='solid') {
      paint.fillStyle=gradePreview.swatch(fx.color||'#272733',grade);
      paint.fillRect((1-size)*width/2+tf.x*width,(1-size)*height/2+tf.y*height,width*size,height*size);
    } else {
      const frame=timelineVideo(clip,at,playing,layers,redraw);
      used.add(clip.id);
      if(frame.readyState>=2 && frame.videoWidth) {
        const fit=Math.min(width/frame.videoWidth,height/frame.videoHeight)*size;
        const dw=frame.videoWidth*fit,dh=frame.videoHeight*fit;
        const graded=gradePreview.paint(frame,grade,Math.min(width,dw),Math.min(height,dh));
        paint.drawImage(graded,(width-dw)/2+tf.x*width,(height-dh)/2+tf.y*height,dw,dh);
      }
    }
    paint.restore();
  }
  if (playing) {
    for (const clip of basePictures()) {
      const start = pictureStart(clip);
      if (start > at && start - at <= ticks(0.4)) {
        timelineVideo(clip,start,false,layers,redraw);
        gradeForPicture(clip,clip.effects.grade);
        used.add(clip.id);
      }
    }
    for (const clip of project.timeline) {
      if (clip.kind !== 'text' || timelineStart(clip.id) <= at || timelineStart(clip.id) - at > ticks(.8)) continue;
      if (!seq.tracks.find(track => track.name === clipTrack(clip))?.hidden) titlePreview(pictureEffects(clip),seq);
    }
  }
  for (const [id, layer] of layers) {
    if (used.has(id)) continue;
    layer.pause();
    if (layers.size > 16) {
      layer.removeAttribute('src');
      layer.load();
      layers.delete(id);
    }
  }
  const crop=cropSettings && exportWindow(width,height,cropSettings.output,cropSettings.reframe);
  const output=captionOutput || [seq.width-seq.width%2,seq.height-seq.height%2];
  const style={...seq.captions,...captionStyle};
  if (playing && seq.captions && !seq.captions.hidden) {
    const next=captions.find(block=>block.start>at && block.start-at<=ticks(.8));
    if (next) captionShape(next.words,style,output);
  }
  if (caption) {
    if (crop) {
      paint.save();
      paint.beginPath();
      paint.rect(crop.x,crop.y,crop.width,crop.height);
      paint.clip();
      paint.translate(crop.x,crop.y);
      drawCaption(caption,style,at,crop.width,crop.height,output,paint);
      paint.restore();
    } else {
      drawCaption(caption,style,at,width,height,output,paint);
    }
  }
  return crop;
}
function captionShape(words,style,[width,height]) {
  if (!words.length) return null;
  const body={words:words.map(word=>word.text),width,height,size:style.size,outline:style.outline};
  return overlayPreview('caption-preview',body);
}

function captionRgba(color) {
  const hex=color.replace(/^#/,'');
  return [0,2,4,6].map(at=>at===6 && hex.length===6 ? 255 : parseInt(hex.slice(at,at+2),16));
}

function colorCaption(entry,style,lit) {
  const key=[style.text_color,style.highlight_color,style.outline_color,lit].join('/');
  if (entry.colorKey===key) return entry.colored;
  if (!entry.colored) {
    const {width,height}=entry.bitmap;
    const bitmap=entry.colored=document.createElement('canvas');
    bitmap.width=width;
    bitmap.height=height;
    const brush=bitmap.getContext('2d',{willReadFrequently:true});
    brush.drawImage(entry.bitmap,0,0);
    entry.mask=brush.getImageData(0,0,width,height).data;
    entry.pixels=brush.createImageData(width,height);
  }
  const body=captionRgba(style.text_color || '#ffffff');
  const accent=captionRgba(style.highlight_color || '#ffd640');
  const border=captionRgba(style.outline_color || '#000000');
  const {mask,pixels,colored}=entry;
  for (let i=0; i<mask.length; i+=4) {
    const text=lit>=0 && mask[i+2]===lit+1 ? accent : body;
    const over=mask[i]*text[3]/255;
    const under=Math.floor(mask[i+1]*border[3]/255)*(1-over/255);
    const alpha=over+under;
    pixels.data[i+3]=Math.round(alpha);
    if (alpha>0) for (let channel=0; channel<3; channel++) {
      pixels.data[i+channel]=Math.round((text[channel]*over+border[channel]*under)/alpha);
    }
  }
  colored.getContext('2d').putImageData(pixels,0,0);
  entry.colorKey=key;
  return colored;
}

function drawCaption(block,style,at,width,height,output,paint) {
  const draft = captionEdit?.block.id===block.id && captionEdit.value;
  const words = draft ? draft.split(' ').map((text,index,items) => ({
    text,start:block.start+(block.end-block.start)*index/items.length
  })) : block.words;
  if (!words?.length) return;
  const lit = style.highlight ? Math.max(0,words.findLastIndex(word => word.start<=at)) : -1;
  const image=captionShape(words,style,output);
  if (image) {
    const x=Math.round((output[0]-image.width)/2)*width/output[0];
    const top=Math.max(0,Math.min(output[1]-image.height,Math.round(output[1]*style.position-image.height/2)));
    paint.drawImage(colorCaption(image,style,lit),x,top*height/output[1],image.width*width/output[0],image.height*height/output[1]);
    return;
  }
  const size = Math.max(1,style.size*Math.min(width,height)/1080);
  paint.save();
  paint.font = '900 '+size+'px system-ui';
  paint.textAlign = 'left';
  paint.textBaseline = 'middle';
  const gap = paint.measureText(' ').width + size*.06;
  const rows = [{items:[],width:0}];
  let row = rows[0];
  words.forEach((word,index) => {
    const span = paint.measureText(word.text).width;
    if (row.items.length && row.width + gap + span > width*.86) {
      row = {items:[],width:0};
      rows.push(row);
    }
    const x = row.width+(row.items.length ? gap : 0);
    row.items.push({text:word.text,index,x});
    row.width = x+span;
  });
  const step = size*1.15;
  const cy = Math.max(rows.length*step/2,Math.min(height-rows.length*step/2,height*style.position));
  paint.lineWidth = Math.max(.6,size*.21);
  paint.strokeStyle = style.outline_color;
  paint.lineJoin = 'round';
  rows.forEach((row,index) => {
    const y=cy+(index-(rows.length-1)/2)*step;
    const x=(width-row.width)/2;
    if (style.outline) for (const word of row.items) paint.strokeText(word.text,x+word.x,y);
    for (const word of row.items) {
      paint.fillStyle=word.index===lit ? style.highlight_color : style.text_color;
      paint.fillText(word.text,x+word.x,y);
    }
  });
  paint.restore();
}
function clearOverlayPreviews() {
  for (const entry of overlayPreviews.values()) {
    entry.controller?.abort();
    entry.bitmap?.close();
  }
  overlayPreviews.clear();
  overlayPreviewBytes = overlayPreviewPending = 0;
}

function freeOverlayPreviews(wantedBytes,wantedEntries,keep) {
  const oldEnough = performance.now() - 750;
  for (const [key,entry] of overlayPreviews) {
    if (overlayPreviews.size+wantedEntries <= maxOverlayPreviews && overlayPreviewBytes+wantedBytes <= maxOverlayPreviewBytes) return true;
    if (key === keep || entry.controller || entry.used > oldEnough) continue;
    entry.bitmap?.close();
    overlayPreviewBytes -= entry.bytes || 0;
    overlayPreviews.delete(key);
  }
  return overlayPreviews.size+wantedEntries <= maxOverlayPreviews && overlayPreviewBytes+wantedBytes <= maxOverlayPreviewBytes;
}

function titlePreview(fx,sequence) {
  if (!fx.title?.trim()) return null;
  const style = fx.transform || {};
  const body = {text:fx.title,font:style.font ?? 'System',size:style.size ?? 110,bold:style.bold !== false,
    width:sequence.width,height:sequence.height};
  return overlayPreview('title-preview',body);
}

function overlayPreview(route,body) {
  const key = JSON.stringify([route,body]);
  const cached = overlayPreviews.get(key);
  if (cached && (!cached.retry || performance.now() < cached.retry)) {
    cached.used = performance.now();
    overlayPreviews.delete(key);
    overlayPreviews.set(key,cached);
    return cached.bitmap ? cached : null;
  }
  if (overlayPreviewPending >= 2 || !freeOverlayPreviews(0,cached ? 0 : 1,key)) return null;
  const controller = new AbortController();
  const entry = {controller,used:performance.now()};
  overlayPreviews.set(key,entry);
  overlayPreviewPending++;
  api(projectUrl('/'+route),{method:'POST',data:body,raw:'response',signal:controller.signal}).then(async response => {
    const [width,height] = (response.headers.get('x-editroom-overlay-size') || '').split(',').map(Number);
    if (!(width > 0 && height > 0)) throw new Error('Overlay size is missing.');
    const bitmap = await createImageBitmap(await response.blob(),{colorSpaceConversion:'none'});
    if (overlayPreviews.get(key) !== entry) { bitmap.close(); return; }
    const bytes = bitmap.width * bitmap.height * (route === 'caption-preview' ? 16 : 8);
    if (!freeOverlayPreviews(bytes,0,key)) {
      bitmap.close();
      entry.retry = performance.now() + 5000;
      return;
    }
    Object.assign(entry,{bitmap,width,height,bytes});
    overlayPreviewBytes += bytes;
  }).catch(error => {
    entry.retry = performance.now() + (error.status === 429 ? 1000 : 5000);
  }).finally(() => {
    if (overlayPreviews.get(key) !== entry) return;
    delete entry.controller;
    overlayPreviewPending--;
    drawComposed();
    drawExportPreview();
  });
  return null;
}

function tintTitle(entry,color) {
  if (color?.toLowerCase() === '#ffffff') return entry.bitmap;
  if (!entry.tinted) {
    entry.tinted = document.createElement('canvas');
    entry.tinted.width = entry.bitmap.width;
    entry.tinted.height = entry.bitmap.height;
  }
  if (entry.color !== color) {
    const {tinted,bitmap} = entry;
    const brush = tinted.getContext('2d');
    brush.clearRect(0,0,tinted.width,tinted.height);
    brush.drawImage(bitmap,0,0);
    brush.globalCompositeOperation = 'source-in';
    brush.fillStyle = color || '#ffffff';
    brush.fillRect(0,0,tinted.width,tinted.height);
    brush.globalCompositeOperation = 'source-over';
    entry.color = color;
  }
  return entry.tinted;
}

function drawTitle(fx,sequence,width,height,color,paint) {
  const text = fx.title;
  if (!text?.trim()) return;
  const style = fx.transform || {};
  const image = titlePreview(fx,sequence);
  if (image) {
    const w = image.width*width/(sequence.width - sequence.width%2);
    const h = image.height*height/(sequence.height - sequence.height%2);
    const x = width*Math.max(0,Math.min(1,style.x ?? .5)) - w/2;
    const y = height*Math.max(0,Math.min(1,style.y ?? .5)) - h/2;
    paint.drawImage(tintTitle(image,color),x,y,w,h);
    return;
  }
  const size=(style.size||110)*height/1080;
  const family=style.font && style.font.toLowerCase()!=='system' ? JSON.stringify(style.font)+', system-ui' : 'system-ui';
  paint.font=(style.bold===false?'400 ':'700 ')+size+'px '+family;
  paint.textAlign='center';paint.textBaseline='middle';paint.fillStyle=color||'white';
  const lines=String(text||'').split('\n');
  for(let i=0;i<lines.length;i++) paint.fillText(lines[i],width*(style.x??.5),height*(style.y??.5)+(i-(lines.length-1)/2)*size*1.13,width*.94);
}
function clipAudioPlayer(clip) {
  let player = audioLayers.get(clip.id);
  if (!player) {
    player = new Audio(clipAudioUrl(clip));
    player.preload = 'auto';
    audioLayers.set(clip.id,player);
  }
  return player;
}
function audibleTimelineClips() {
  const tracks = project.sequence.tracks;
  const solo = tracks.some(track => track.name.startsWith('A') && track.solo);
  const sources = [];
  for (const clip of project.timeline) {
    const media=source(clip.source);
    if (media?.missing_file) continue;
    const embedded = clip.kind === 'video' && media?.audio && !clip.link && !clip.audio_detached;
    if ((clip.kind !== 'audio' && !embedded) || clip.mute || clip.effects.freeze) continue;
    const track = tracks.find(track => track.name === clipTrack(clip));
    if (!track || (!embedded && track.muted) || (solo && (embedded || !track.solo))) continue;
    sources.push({clip,track,embedded});
  }
  return sources;
}
function syncLayerAudio(at, scrubbing = false) {
  const used = new Set();
  for (const {clip,track,embedded} of audibleTimelineClips()) {
    const start = timelineStart(clip.id);
    if (at < start || at >= start + length(clip)) {
      if (clip.timed_audio && at < start && start - at < ticks(3)) clipAudioPlayer(clip);
      continue;
    }
    const player = clipAudioPlayer(clip);
    used.add(clip.id);
    const since = at - start;
    const offset = (clip.timed_audio ? since : clip.start + since) / TICKS_PER_SECOND;
    if (player.readyState === 0) {
      player.onloadedmetadata = () => {
        if (!playingCut && !exportPreview?.playing && !scrubbing) return;
        player.currentTime = Math.min(offset, Math.max(0, player.duration - .001));
      };
    } else if ((scrubbing || Math.abs(player.currentTime-offset) > .12) && !player.seeking) {
      player.currentTime = Math.min(offset, Math.max(0, player.duration - .001));
    }
    const gain = clipFade(clip,since) * 10**((pictureEffects(clip).gain_db + (embedded ? 0 : trackGain(track)))/20);
    setPlayerGain(player, embedded ? 'embedded' : track.name, gain * duckGain(duckMix[track.name], at));
    if(player.paused)player.play().catch(()=>{});
  }
  for(const [id,player] of audioLayers)if(!used.has(id) && !player.paused)player.pause();
}

function meter(name, label) {
  const element = item('div','audio-meter','');
  element.dataset.meter = name;
  element.setAttribute('role','meter');
  element.setAttribute('aria-label',label);
  element.setAttribute('aria-valuemin','-60');
  element.setAttribute('aria-valuemax','0');
  element.setAttribute('aria-valuenow','-60');
  element.append(item('i','meter-peak',''));
  return element;
}
byId('master-meters').append(meter('left','Left output level'), meter('right','Right output level'));

function stereoBus() {
  const gain = audioContext.createGain();
  gain.channelCount = 2;
  gain.channelCountMode = 'explicit';
  const split = audioContext.createChannelSplitter(2);
  gain.connect(split);
  const meters = [0,1].map(channel => {
    const analyser = audioContext.createAnalyser();
    analyser.fftSize = 2048;
    split.connect(analyser,channel);
    return {analyser,data:new Float32Array(analyser.fftSize)};
  });
  return {gain, split, meters};
}
function startAudioContext() {
  if (!audioContext && window.AudioContext) {
    audioContext = new AudioContext();
    masterBus = stereoBus();
    masterBus.gain.connect(audioContext.destination);
  }
  if (audioContext?.state === 'suspended') audioContext.resume().catch(() => {});
}
function setPlayerGain(player, track, level) {
  if (!audioContext) { player.volume = Math.min(1,level); return; }
  let bus = trackBuses.get(track);
  if (!bus) {
    bus = stereoBus();
    bus.gain.connect(masterBus.gain);
    trackBuses.set(track,bus);
  }
  let wiring = playerSignals.get(player);
  if (!wiring) {
    const signal = audioContext.createMediaElementSource(player);
    const gain = audioContext.createGain();
    signal.connect(gain);
    player.volume = 1;
    wiring = {signal,gain,bus:null};
    playerSignals.set(player,wiring);
  }
  if (wiring.bus !== bus) {
    wiring.gain.disconnect();
    wiring.gain.connect(bus.gain);
    wiring.bus = bus;
  }
  wiring.gain.gain.setTargetAtTime(level,audioContext.currentTime,.008);
}
function peakOf(channel) {
  channel.analyser.getFloatTimeDomainData(channel.data);
  let peak = 0;
  for (const value of channel.data) peak = Math.max(peak,Math.abs(value));
  return peak;
}
function paintMeter(element, peak, now) {
  const raw = peak > 0 ? Math.max(-60,20*Math.log10(peak)) : -60;
  const db = Math.min(0,raw);
  const previous = meterReadings.get(element) || {level:-60,peak:-60,until:0,at:now};
  const scale = Math.min(3,Math.max(1,(now - previous.at)/16.67));
  const level = Math.max(db,previous.level - 4 * scale);
  const top = db >= previous.peak ? db : (now > previous.until ? Math.max(db,previous.peak - 2 * scale) : previous.peak);
  const until = db >= previous.peak ? now + 500 : previous.until;
  meterReadings.set(element,{level,peak:top,until,at:now});
  element.style.setProperty('--level', (1 + level/60)*100 + '%');
  element.style.setProperty('--peak', Math.min(98,(1 + top/60)*100) + '%');
  element.style.setProperty('--peak-visible', Number(top > -59.5));
  element.dataset.clipped = String(raw >= -.1);
  element.setAttribute('aria-valuenow',String(Math.round(db)));
}
function updateMeters(now) {
  if (!masterBus) return;
  for (const element of document.querySelectorAll('[data-meter]')) {
    const key = element.dataset.meter;
    const bus = trackBuses.get(key);
    const peak = key === 'left' || key === 'right'
      ? peakOf(masterBus.meters[key === 'left' ? 0 : 1])
      : bus ? Math.max(...bus.meters.map(peakOf)) : 0;
    paintMeter(element,peak,now);
  }
}
function clearMeters() {
  for (const element of document.querySelectorAll('[data-meter]')) {
    meterReadings.delete(element);
    paintMeter(element,0,performance.now());
  }
}
async function refreshMix() {
  mixRequest?.abort();
  const id = projectId;
  const revision = project.revision;
  const query=previewQuery();
  if (!project.sequence.tracks.some(track => track.duck_under)) { duckMix = {}; return; }
  const request = new AbortController();
  mixRequest = request;
  try {
    const mix = await api(projectUrl('/audio-mix'+query),{signal:request.signal});
    if (projectId === id && project.revision === revision && mix.revision === revision && query===previewQuery()) duckMix = mix.tracks;
  } catch (error) {
    if (!request.signal.aborted && projectId === id) status(error.message);
  }
}
function duckGain(curve, at) {
  if (!curve) return 1;
  const spans = curve.spans;
  let lo = 0;
  let hi = spans.length;
  while (lo < hi) {
    const middle = (lo + hi) >> 1;
    if (spans[middle][0] <= at) lo = middle + 1;
    else hi = middle;
  }
  const tail = lo > 0 ? Math.max(0, at - spans[lo - 1][1]) / (TICKS_PER_SECOND * .5) : 1;
  const lead = lo < spans.length ? (spans[lo][0] - at) / (TICKS_PER_SECOND * .15) : 1;
  return curve.low + (1 - curve.low) * Math.min(1,tail,lead);
}
function openDucking(track) {
  duckTrack = track.id;
  byId('duck-title').textContent = track.name + ' ducking';
  const others = project.sequence.tracks.filter(other => other.name.startsWith('A') && other.id !== track.id);
  const voice = byId('duck-voice');
  voice.replaceChildren(option('None',''), ...others.map(other => {
    const entry=option(other.name,other.name);
    entry.dataset.trackId=other.id;
    return entry;
  }));
  voice.value = others.find(other => other.id === track.duck_under)?.name || '';
  const db = byId('duck-db');
  const steps = [...new Set([6,12,18,24,-track.duck_db])].sort((a,b) => a-b);
  db.replaceChildren(...steps.map(value => option(value + ' dB',-value)));
  db.value = track.duck_db;
  byId('duck-dialog').showModal();
}
byId('duck-cancel').onclick = () => byId('duck-dialog').close();
byId('duck-form').onsubmit = async event => {
  event.preventDefault();
  const changed = await mcp('set_track',{track:duckTrack,duck_under:byId('duck-voice').selectedOptions[0]?.dataset.trackId || null,duck_db:Number(byId('duck-db').value)});
  if (changed) byId('duck-dialog').close();
};
byId('even-levels').onclick = async () => {
  const clip_ids=project.timeline.filter(clip=>clip.kind==='audio' && selection.has(clip.id)).map(clip=>clip.id);
  const args=clip_ids.length ? {clip_ids} : {};
  status('Measuring audio…');
  const result = await mcp('even_out_audio_levels',args);
  if (result) status(result.adjusted.length ? 'Audio levels saved' : 'Audio levels are already even or there is no audible clip.');
};
function scrubAudio(at) {
  if (playingCut || !toggled('audio-scrub') || performance.now() - lastScrub < 65) return;
  lastScrub = performance.now();
  startAudioContext();
  clearTimeout(scrubTimer);
  syncLayerAudio(at,true);
  syncSoundtrack(at);
  updateMeters(lastScrub);
  scrubTimer = setTimeout(() => {
    if (!playingCut) {
      pauseLayerAudio();
      soundtrack.pause();
      clearMeters();
    }
  },85);
}

byId('auto-edit').onclick = async () => {
  if (byId('auto-edit').disabled) return;
  byId('auto-edit').disabled=true;
  byId('director-error').textContent='';
  const editor=projectId;
  try {
    const run=await post(projectUrl('/auto-edit'),{});
    if (projectId===editor) shownRun=run.id;
    await refresh();
  } catch (error) {
    if (projectId===editor) byId('director-error').textContent=error.message;
    renderActivity();
  }
};

byId('stop-run').onclick = async () => {
  const run=state.runs.find(item=>item.project===projectId && item.status==='running' && !item.applied);
  if (!run) return;
  stoppingRun=run.id;
  closeDirectorPreview();
  renderStudio(state);
  try {await post('/api/projects/'+run.project+'/director/stop');await refresh();}
  catch(error){byId('director-error').textContent=error.message;}
  finally {stoppingRun=null;}
};

const builtInLooks = ['Warm film','Golden hour','Film print','Teal and orange','Pastel','Bleach bypass','Mono','Cool night'];
const gradeFields = ['exposure','contrast','saturation','temperature','tint'];
const wheels = ['shadows','midtones','highlights'];

function pictureEffects(clip) {
  return pictureDraft?.clip === clip.id ? {...clip.effects,...pictureDraft.effects} : clip.effects;
}
function gradeInput(raw) {
  if (!raw) return null;
  const grade = {};
  if (raw.look && (builtInLooks.includes(raw.look) || raw.look === 'Faded' || raw.look.startsWith('lut:')) &&
      raw.look_intensity !== 0) {
    grade.look = raw.look;
    if (raw.look_intensity != null && raw.look_intensity !== 1) grade.look_intensity = raw.look_intensity;
  }
  for (const field of gradeFields) if (raw[field]) grade[field] = raw[field];
  for (const wheel of wheels) {
    const point = raw[wheel];
    if (point?.x || point?.y) grade[wheel] = {x:point.x || 0,y:point.y || 0};
  }
  return Object.keys(grade).length ? grade : null;
}
function cachedGradeKey(grade) {return grade ? projectId + '/' + JSON.stringify(grade) : null;}
async function loadGrade(request) {
  const {key,grade,editor} = request;
  if (gradePending.has(key) || gradeCache.has(key)) return;
  gradePending.add(key);
  try {
    const blob = await api('/api/projects/'+editor+'/grade-preview',{method:'POST',data:grade,raw:true});
    const bytes = new Uint8Array(await blob.arrayBuffer());
    if (bytes.length !== 3*gradePreview.grid**3) {gradeCache.set(key,null);return;}
    gradeCache.set(key,{bytes});
    if (gradeCache.size>32) {
      const first=gradeCache.keys().next().value;
      const entry=gradeCache.get(first);
      if (entry) gradePreview.free(entry);
      gradeCache.delete(first);
    }
  } catch (_) {
    gradeCache.set(key,null);
  } finally {
    gradePending.delete(key);
    if (projectId === editor) {
      drawComposed();
      scheduleTimelineMedia();
    }
  }
}
function runGradeDraft() {
  gradeDraftTimer = null;
  const request = queuedGradeDraft;
  if (!request || draftGradeRequest || projectId !== request.editor) return;
  queuedGradeDraft = null;
  draftGradeRequest = loadGrade(request).finally(() => {
    if (gradeCache.get(request.key) && pictureDraft?.clip === request.clip) {
      pictureDraft.lastGradeKey = request.key;
      drawComposed();
    }
    draftGradeRequest = null;
    if (queuedGradeDraft) gradeDraftTimer = setTimeout(runGradeDraft,36);
  });
}
function gradeForPicture(clip,raw) {
  const grade = gradeInput(raw);
  if (!grade) return null;
  const key = cachedGradeKey(grade);
  if (gradeCache.has(key)) return gradeCache.get(key);
  const editor = projectId;
  if (pictureDraft?.clip === clip.id && pictureDraft.effects.grade === raw) {
    if (!gradePending.has(key)) {
      queuedGradeDraft = {key,grade,editor,clip:clip.id};
      if (!draftGradeRequest && !gradeDraftTimer) gradeDraftTimer = setTimeout(runGradeDraft,25);
    }
    return gradeCache.get(pictureDraft.lastGradeKey) || gradeCache.get(cachedGradeKey(gradeInput(clip.effects.grade)));
  }
  if (!gradePending.has(key)) loadGrade({key,grade,editor});
  return null;
}
function setPictureDraft(clip,effects) {
  const prior = pictureDraft?.clip === clip.id ? pictureDraft.lastGradeKey : null;
  pictureDraft = {clip:clip.id,effects,lastGradeKey:prior};
  drawComposed();
  scheduleTimelineMedia();
  return pictureDraft;
}
function clearPictureDraft(entry) {
  if (entry && pictureDraft === entry) {
    pictureDraft = null;
    drawComposed();
    scheduleTimelineMedia();
  }
}
function savePicture(tool,args,entry) {
  const editor = projectId;
  const write = () => mcp(tool,args,{editor});
  pictureWrites = pictureWrites.then(write,write);
  return pictureWrites.then(result => {
    clearPictureDraft(entry);
    return result;
  });
}

function zoomAmount(zoom,elapsed) {
  if (!zoom) return 1;
  function between(first,last) {
    if (last-first<=1e-6) return elapsed>=first ? 1 : 0;
    const k=Math.max(0,Math.min(1,(elapsed-first)/(last-first)));
    return zoom.smooth === false ? k : k*k*(3-2*k);
  }
  if (zoom.mode==='out') return 1-between(zoom.p3,zoom.p4);
  if (zoom.mode==='inOut' && elapsed>=zoom.p3) return 1-between(zoom.p3,zoom.p4);
  return between(zoom.p1,zoom.p2);
}
function transformAt(style = null,elapsed = 0) {
  const scale=style?.scale ?? 1;
  const x=style?.x ?? 0;
  const y=style?.y ?? 0;
  const k=zoomAmount(style?.zoom,elapsed);
  const m=(scale-1)/2;
  const limit=v=>scale>1.001 ? Math.max(-m,Math.min(m,v)) : v;
  return {scale:1+(scale-1)*k,x:limit(x)*k,y:limit(y)*k};
}
function showZoomed(clip) {
  const zoom = clip?.effects.transform?.zoom;
  if (playingCut || !zoom) return;
  const start = timelineStart(clip.id);
  const elapsed = (frameAt-start)/TICKS_PER_SECOND;
  if (elapsed>=0 && frameAt<timelineEnd(clip) && zoomAmount(zoom,elapsed)>.999) return;
  const point = zoom.mode==='out' ? Math.max(0,zoom.p3-0.001) : zoom.p2;
  seekTimeline(start+Math.min(length(clip)-frameStep(),ticks(point)));
}

const graph = byId('zoom-graph');
const graphBox = byId('zoom-controls');
const graphHeight = 82;
const graphPoints = Object.fromEntries(['p1','p2','p3','p4'].map(key => {
  const button=item('button','zoom-point','');
  button.type='button';
  button.setAttribute('aria-label',{
    p1:'Start zoom in',p2:'Finish zoom in',p3:'Start zoom out',p4:'Finish zoom out'
  }[key]);
  button.onpointerdown = event => dragZoomGraph(event,key);
  button.onkeydown = event => nudgeZoomPoint(event,key);
  byId('zoom-points').append(button);
  return [key,button];
}));
let keptGraphTop;
let activeGraphClip;
const graphTopFor = scale => [3,5,10].find(top=>scale < top*.97) ?? 10;
const graphY = (scale,top) => 6+(graphHeight-12)*(1-Math.min(1,Math.log(Math.max(1,scale))/Math.log(top)));
function graphX(time,duration,width) {return Math.max(6,Math.min(width-6,(time/duration)*width));}
function drawZoomGraph(clip,style=pictureEffects(clip).transform,forcedTop) {
  const zoom=style?.zoom;
  graphBox.hidden = !zoom;
  byId('clip-zoom').setAttribute('aria-pressed',String(!!zoom));
  if (!zoom) return;
  if (activeGraphClip!==clip.id) {
    activeGraphClip=clip.id;
    keptGraphTop=graphTopFor(style.scale);
  }
  keptGraphTop=Math.max(keptGraphTop,graphTopFor(style.scale));
  const top=forcedTop ?? keptGraphTop;
  const width=graph.clientWidth;
  if (!width) return;
  const scale=Math.max(1,style.scale);
  const lengthSeconds=length(clip)/TICKS_PER_SECOND;
  const context=graph.getContext('2d');
  const ratio=window.devicePixelRatio || 1;
  graph.width=Math.round(width*ratio);
  graph.height=Math.round(graphHeight*ratio);
  context.scale(ratio,ratio);
  const baseline=graphY(1,top);
  context.strokeStyle='#cdb9e65c';
  context.setLineDash([3,3]);
  context.beginPath();context.moveTo(0,baseline);context.lineTo(width,baseline);context.stroke();
  context.setLineDash([]);
  context.strokeStyle='#e0cdf9';
  context.lineWidth=1.5;
  context.beginPath();
  for (let i=0;i<=100;i++) {
    const x=i*width/100;
    const y=graphY(Math.pow(scale,zoomAmount(zoom,i*lengthSeconds/100)),top);
    if (i===0) context.moveTo(x,y);
    else context.lineTo(x,y);
  }
  context.stroke();
  for (const [key,button] of Object.entries(graphPoints)) {
    button.hidden = zoom.mode==='in' ? ['p3','p4'].includes(key) : zoom.mode==='out' && ['p1','p2'].includes(key);
    button.style.left=graphX(zoom[key],lengthSeconds,width)+'px';
    button.style.top=graphY(key==='p1' || key==='p4' ? 1 : scale,top)+'px';
    button.title=tc(ticks(zoom[key]));
  }
  for (const button of graphBox.querySelectorAll('[data-zoom-mode]')) {
    button.setAttribute('aria-pressed',String(zoom.mode === button.dataset.zoomMode));
  }
  byId('zoom-ease').setAttribute('aria-pressed',String(zoom.smooth !== false));
}
new ResizeObserver(() => {
  const clip=project && zoomInspectorClip();
  if (clip && !graphBox.hidden) drawZoomGraph(clip);
}).observe(graph);

function constrainZoomPoint(zoom,key,value,clip) {
  const duration=length(clip)/TICKS_PER_SECOND;
  const gap=frameStep()/TICKS_PER_SECOND;
  if (key==='p1') return Math.max(0,Math.min(value,zoom.p2-gap));
  if (key==='p2') return Math.min(Math.max(value,zoom.p1+gap),zoom.mode==='inOut' ? zoom.p3 : duration);
  if (key==='p3') return Math.max(Math.min(value,zoom.p4-gap),zoom.mode==='inOut' ? zoom.p2 : 0);
  return Math.min(duration,Math.max(value,zoom.p3+gap));
}
function nudgeZoomPoint(event,key) {
  if (!['ArrowRight','ArrowLeft'].includes(event.key)) return;
  event.preventDefault();
  event.stopPropagation();
  const clip=zoomInspectorClip();
  if (!clip || trackFor(clip)?.locked) return;
  const tf=pictureEffects(clip).transform;
  if (!tf?.zoom) return;
  const offset=(event.key==='ArrowRight'?1:-1)*frameStep()/TICKS_PER_SECOND*(event.shiftKey?5:1);
  const zoom={...tf.zoom,[key]:constrainZoomPoint(tf.zoom,key,tf.zoom[key]+offset,clip)};
  seekTimeline(timelineStart(clip.id)+ticks(zoom[key]));
  savePicture('set_transform',{clip_id:clip.id,zoom},setPictureDraft(clip,{transform:{...tf,zoom}}));
}
function dragZoomGraph(event,key) {
  if (event.button !== 0) return;
  const clip=zoomInspectorClip();
  if (!clip || trackFor(clip)?.locked) return;
  const initial=pictureEffects(clip).transform;
  if (!initial?.zoom) return;
  event.preventDefault();
  event.stopPropagation();
  const box=graph.getBoundingClientRect(), top=keptGraphTop;
  const duration=length(clip)/TICKS_PER_SECOND;
  const [oldX,oldY]=[event.clientX,event.clientY];
  const editor=projectId;
  const before=frameAt;
  let scrubbed=false;
  let draft=null;
  let latest=initial;
  function update(pointer,preview=true) {
    if (pointer.clientX===oldX && pointer.clientY===oldY && !draft) return;
    const zoom={...initial.zoom};
    let scale=initial.scale;
    if (key==='scale') {
      const y=(pointer.clientY-box.top-6)/(graphHeight-12);
      scale=Math.max(1.05,Math.round(Math.pow(top,Math.min(1,Math.max(0,1-y)))*100)/100);
    } else {
      const fraction=Math.max(0,Math.min(1,(pointer.clientX-box.left)/box.width));
      const time=onFrame(ticks(duration*fraction))/TICKS_PER_SECOND;
      zoom[key]=constrainZoomPoint(zoom,key,time,clip);
      if (preview && !playingCut && !waitingForAudio) {
        scrubbed=true;
        placePlayhead(timelineStart(clip.id)+ticks(zoom[key]));
      }
    }
    latest={...initial,scale,zoom};
    draft=setPictureDraft(clip,{transform:latest});
    transformFields(latest);
    drawZoomGraph(clip,latest,top);
  }
  timelineGesture(event,{
    cancelOnEscape:true,
    update,
    complete(pointer,moved) {
      if (moved) {
        update(pointer,false);
        savePicture('set_transform',{clip_id:clip.id,scale:latest.scale,zoom:latest.zoom},draft);
      }
    },
    cancel(committing) {
      clearPictureDraft(draft);
      if (projectId!==editor) return;
      if (scrubbed && !playingCut && !waitingForAudio) placePlayhead(before);
      if (!committing && zoomInspectorClip()?.id===clip.id) {
        transformFields(initial);
        drawZoomGraph(clip,initial);
      }
    }
  });
}
graph.onpointerdown=event => {
  const clip=zoomInspectorClip();
  if (!clip) return;
  const transform=pictureEffects(clip).transform;
  if (!transform?.zoom) return;
  const box=graph.getBoundingClientRect();
  const time=(event.clientX-box.left)/box.width*length(clip)/TICKS_PER_SECOND;
  const y=event.clientY-box.top;
  if (zoomAmount(transform.zoom,time)>.999 && Math.abs(y-graphY(transform.scale,keptGraphTop))<12) {
    dragZoomGraph(event,'scale');
  }
};
let lastZoomMode = sessionStorage.getItem('editroom.zoom-mode') || 'in';
byId('clip-zoom').onclick=async () => {
  const clip=zoomInspectorClip();
  if (!clip) return;
  const zoom=clip.effects.transform?.zoom;
  const editor=projectId;
  const response=await savePicture('set_transform',{clip_id:clip.id,zoom:zoom ? 'off' : lastZoomMode});
  if (response && projectId===editor) showZoomed(zoomInspectorClip());
};
for (const button of graphBox.querySelectorAll('[data-zoom-mode]')) button.onclick = async () => {
  const clip=zoomInspectorClip();
  const zoom=clip?.effects.transform?.zoom;
  if (!zoom || zoom.mode===button.dataset.zoomMode) return;
  lastZoomMode=button.dataset.zoomMode;
  sessionStorage.setItem('editroom.zoom-mode',lastZoomMode);
  const editor=projectId;
  if (await savePicture('set_transform',{clip_id:clip.id,zoom:lastZoomMode,zoom_smooth:zoom.smooth !== false}) &&
      projectId===editor) showZoomed(zoomInspectorClip());
};
byId('zoom-ease').onclick=() => {
  const clip=zoomInspectorClip();
  const zoom=clip?.effects.transform?.zoom;
  if (zoom) savePicture('set_transform',{clip_id:clip.id,zoom_smooth:zoom.smooth===false});
};

const wheelButtons = Object.fromEntries(wheels.map(wheel => {
  const name=wheel[0].toUpperCase()+wheel.slice(1);
  const box=item('div','wheel-box','');
  const button=item('div','grade-wheel','');
  button.tabIndex=0;
  button.setAttribute('role','slider');
  button.setAttribute('aria-label',name+' wheel');
  button.setAttribute('aria-valuemin','0');
  button.setAttribute('aria-valuemax','100');
  button.append(item('span','wheel-point',''));
  button.onpointerdown=event=>dragGradeWheel(event,wheel);
  button.ondblclick=()=>editGrade({[wheel]:{x:0,y:0}});
  button.onkeydown=event=>{
    const delta={ArrowLeft:[-1,0],ArrowRight:[1,0],ArrowUp:[0,-1],ArrowDown:[0,1]}[event.key];
    if (!delta && event.key!=='Home' && event.key!=='Enter') return;
    event.preventDefault();
    const clip=inspectorAnchor(selectedPictures());
    if (!clip || trackFor(clip)?.locked) return;
    const current=pictureEffects(clip).grade?.[wheel] || {x:0,y:0};
    const step=event.shiftKey ? .1 : .02;
    const point=delta ? wheelPoint(current.x+delta[0]*step,current.y+delta[1]*step) : {x:0,y:0};
    editGrade({[wheel]:point});
  };
  box.append(button,item('span','',name));
  byId('grade-wheels').append(box);
  return [wheel,button];
}));
function wheelPoint(x,y) {
  const radius=Math.max(1,Math.hypot(x,y));
  const snap=value=>Math.round(value*100)/100;
  return {x:snap(x/radius),y:snap(y/radius)};
}
function drawWheels(grade) {
  for (const [key,wheel] of Object.entries(wheelButtons)) {
    const {x = 0,y = 0}=grade?.[key] || {};
    const dot=wheel.firstElementChild;
    dot.style.left=50+x*41+'%';
    dot.style.top=50+y*41+'%';
    wheel.setAttribute('aria-valuenow',String(Math.round(100*Math.hypot(x,y))));
    wheel.setAttribute('aria-valuetext',Math.round(x*100)+'% horizontal, '+Math.round(y*100)+'% vertical');
  }
}
function selectedPictures() {
  return selectedClips().filter(clip=>clip.kind!=='audio');
}
function onlySelectedPicture() {
  const clips=selectedPictures();
  return clips.length===1 ? clips[0] : null;
}
function editablePictures() {
  return selectedPictures().filter(clip=>!trackFor(clip)?.locked);
}
function selectedTransforms() {
  return selectedPictures().filter(clip=>clip.kind!=='text');
}
function zoomInspectorClip() {
  const clips=selectedTransforms();
  return clips.length===1 ? clips[0] : null;
}
function editGrade(args,entry) {
  const clip_ids = editablePictures().map(clip=>clip.id);
  if (clip_ids.length) return savePicture('set_grade',{clip_ids,...args},entry);
}
function dragGradeWheel(event,key) {
  const clip=inspectorAnchor(selectedPictures());
  if (event.button!==0 || !clip || trackFor(clip)?.locked) return;
  const box=event.currentTarget.getBoundingClientRect();
  event.currentTarget.focus();
  event.preventDefault();
  const local=clip.effects.grade || {};
  let draft, point;
  function update(pointer) {
    point=wheelPoint((pointer.clientX-box.left-box.width/2)/(box.width*.41),
      (pointer.clientY-box.top-box.height/2)/(box.height*.41));
    const grade={...local,[key]:point};
    draft=setPictureDraft(clip,{grade});
    drawWheels(grade);
  }
  timelineGesture(event,{
    update,
    complete(pointer) {update(pointer);editGrade({[key]:point},draft);},
    cancel() {clearPictureDraft(draft);drawWheels(clip.effects.grade);}
  });
}

let titleFontsRequest;
function loadTitleFonts() {
  titleFontsRequest ??= api('/api/title-fonts').then(families => {
    byId('title-fonts').replaceChildren(...families.map(family => option(family,family)));
  }).catch(() => { titleFontsRequest = undefined; });
}

function transformFields(style={}) {
  const scale=style.scale ?? 1;
  const fills=scale>1.001;
  const reach=fills ? (scale-1)/2 : (1+scale)/2;
  byId('clip-scale').value=Number((scale*100).toFixed(3));
  for (const [key,size] of [['x',project.sequence.width],['y',project.sequence.height]]) {
    const field=byId('clip-'+key);
    const displacement=(style[key] ?? 0)*size*(fills ? -1 : 1);
    const rounded=Number(displacement.toFixed(3));
    field.max=Math.max(reach*size,Math.abs(rounded));
    field.min=-Number(field.max);
    field.value=rounded;
  }
}
function renderPictureControls() {
  const pictures=selectedPictures();
  const clip=inspectorAnchor(pictures);
  const panel=byId('picture-controls');
  panel.hidden=!clip;
  panel.disabled=!editablePictures().length;
  if (!clip) return;
  const fx=pictureEffects(clip);
  const transforms=selectedTransforms();
  const transformClip=inspectorAnchor(transforms);
  const onlyPicture=onlySelectedPicture();
  const title=onlyPicture?.kind==='text';
  byId('transform-controls').hidden=!transformClip;
  byId('title-controls').hidden=!title;
  renderTransitionControls(onlyPicture);
  byId('solid-color-row').hidden=onlyPicture?.kind!=='solid';
  if (title) {
    const tf=fx.transform||{};
    loadTitleFonts();
    byId('clip-title-text').value=fx.title;
    byId('clip-title-font').value=tf.font || 'System';
    byId('clip-title-size').value=tf.size ?? 110;
    byId('clip-title-bold').checked=tf.bold !== false;
    byId('clip-title-color').value=titleColor(fx);
    for (const key of ['x','y']) byId('clip-title-'+key).value=tf[key] ?? .5;
  }
  if (transformClip) {
    const editable=transforms.filter(item=>!trackFor(item)?.locked);
    const transform=pictureEffects(transformClip).transform || {};
    const zoom=transforms.length===1;
    for (const key of ['scale','x','y']) byId('clip-'+key).disabled=!editable.length;
    byId('clip-zoom').hidden=!zoom;
    byId('clip-zoom').disabled=!editable.length;
    byId('reset-transform').disabled=!editable.some(item=>{
      const value=item.effects.transform;
      return value && ((value.scale ?? 1)!==1 || value.x || value.y || value.zoom);
    });
    transformFields(transform);
    if (onlyPicture?.kind==='solid') byId('clip-solid-color').value=fx.color;
    if (zoom) drawZoomGraph(transformClip,transform);
    else graphBox.hidden=true;
  } else {
    graphBox.hidden=true;
  }
  const grade=fx.grade||{};
  const looks=byId('clip-look');
  const local=project.sequence.luts.map(item=>option(item.name,'lut:'+item.id));
  if (grade.look && !builtInLooks.includes(grade.look) && !local.some(item=>item.value===grade.look)) {
    local.push(option(grade.look.split('/').at(-1).replace(/\.cube$/i,''),grade.look));
  }
  looks.replaceChildren(option('None','none'),...builtInLooks.map(label=>option(label,label)),...local);
  looks.value=grade.look||'none';
  byId('clip-look-intensity').value=grade.look_intensity??1;
  for (const key of gradeFields) byId('clip-'+key).value=grade[key]??0;
  drawWheels(grade);
  byId('copy-grade').disabled = editablePictures().length !== 1;
  byId('paste-grade').disabled = copiedGrade === undefined || !editablePictures().length;
}
function editTransform(settings) {
  const clip_ids=selectedTransforms().filter(clip=>!trackFor(clip)?.locked).map(clip=>clip.id);
  if (clip_ids.length) return savePicture('set_transform',{clip_ids,...settings});
}
for (const key of ['scale','x','y']) byId('clip-'+key).onchange=async event=>{
  const input=event.target;
  const clip=inspectorAnchor(selectedTransforms());
  if (!clip || !input.value || !input.reportValidity()) return;
  const fills=(pictureEffects(clip).transform?.scale ?? 1)>1.001;
  const dimension=key==='x' ? project.sequence.width : project.sequence.height;
  const value=key==='scale' ? input.valueAsNumber/100 : input.valueAsNumber/dimension*(fills ? -1 : 1);
  const editor=projectId;
  if (await editTransform({[key]:value}) && projectId===editor) showZoomed(zoomInspectorClip());
};
byId('reset-transform').onclick=()=>editTransform({reset:true});
for (const key of ['text','font','size','bold','color','x','y']) byId('clip-title-'+key).onchange=event=>{
  const input=event.target;
  const value=input.type==='checkbox' ? input.checked : input.type==='number' ? Number(input.value) : input.value;
  const clip=onlySelectedPicture();
  if (clip?.kind==='text') savePicture('set_title_style',{clip_id:clip.id,[key]:value});
};
byId('clip-solid-color').onchange=event=>{
  const clip=onlySelectedPicture();
  if (clip?.kind==='solid') savePicture('set_solid_color',{clip_id:clip.id,color:event.target.value});
};
byId('clip-look').onchange=event=>editGrade({look:event.target.value});
byId('clip-look-intensity').onchange=event=>editGrade({look_intensity:Number(event.target.value)});
for (const key of gradeFields) byId('clip-'+key).onchange=event=>editGrade({[key]:Number(event.target.value)});
function copySelectedGrade() {
  const clips=editablePictures();
  if (clips.length!==1) return;
  const [clip]=clips;
  copiedGrade=clip.effects.grade ? structuredClone(clip.effects.grade) : null;
  byId('paste-grade').disabled=false;
  status('Grade copied');
}
function pasteSelectedGrade() {
  if (copiedGrade !== undefined) editGrade({replace:copiedGrade});
}
byId('copy-grade').onclick=copySelectedGrade;
byId('paste-grade').onclick=pasteSelectedGrade;
byId('reset-grade').onclick=()=>editGrade({reset:true});
byId('import-lut').onchange=async event=>{
  const file=event.target.files[0];
  if (!file) return;
  event.target.value='';
  const editor=projectId;
  const ids=editablePictures().map(clip=>clip.id);
  try {
    const lut=await api(projectUrl('/lut'),{method:'POST',file});
    await mcp('set_grade',{clip_ids:ids,look:'lut:'+lut.id},{editor});
  } catch (error) {status(error.message);}
};

viewerPicture.onpointerdown = event => {
  if (event.button!==0 || sourcePreview || compositeCanvas.hidden || !compositeCanvas.width ||
      (event.pointerType==='touch' && viewerViewport.hasAttribute('data-scrollable'))) return;
  const clip=onlySelectedPicture();
  if (!clip || trackFor(clip)?.locked) return;
  const rect=compositeCanvas.getBoundingClientRect();
  const aspect=compositeCanvas.width/compositeCanvas.height;
  const width=Math.min(rect.width,rect.height*aspect), height=width/aspect;
  if (Math.abs(event.clientX-rect.left-rect.width/2)>width/2 ||
      Math.abs(event.clientY-rect.top-rect.height/2)>height/2) return;
  const isTitle=clip.kind==='text';
  const style=clip.effects.transform || {};
  const start=isTitle ? {x:style.x ?? .5,y:style.y ?? .5} : transformAt({...style,zoom:null});
  const limit=(style.scale ?? 1)>1.001?(style.scale-1)/2:2;
  const editor=projectId;
  const revision=project.revision;
  let draft, x, y, started=false;
  function drag(pointer) {
    if (!started) {started=true;if (style.zoom) showZoomed(clip);}
    x=start.x+(pointer.clientX-event.clientX)/width;
    y=start.y+(pointer.clientY-event.clientY)/height;
    if (isTitle) {
      x=Math.max(0,Math.min(1,x));
      y=Math.max(0,Math.min(1,y));
    } else {
      if (Math.abs(x*width)<5) x=0;
      if (Math.abs(y*height)<5) y=0;
      x=Math.round(Math.max(-limit,Math.min(limit,x))*project.sequence.width)/project.sequence.width;
      y=Math.round(Math.max(-limit,Math.min(limit,y))*project.sequence.height)/project.sequence.height;
    }
    draft=setPictureDraft(clip,{transform:{...style,x,y}});
    if (isTitle) {
      byId('clip-title-x').value=x;
      byId('clip-title-y').value=y;
    } else transformFields({...style,x,y});
  }
  timelineGesture(event,{
    cancelOnEscape:true,
    update:drag,
    complete(pointer,moved) {
      if (moved) {
        drag(pointer);
        savePicture(isTitle?'set_title_style':'set_transform',{clip_id:clip.id,x,y},draft);
      }
    },
    cancel(committing) {
      clearPictureDraft(draft);
      if (committing || projectId!==editor || project.revision!==revision || onlySelectedPicture()?.id!==clip.id) return;
      if (isTitle) {
        byId('clip-title-x').value=start.x;
        byId('clip-title-y').value=start.y;
      } else transformFields(style);
    }
  });
};
