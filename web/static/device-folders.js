class DeviceFolders {
  static canSync() {
    return window.isSecureContext && Boolean(window.indexedDB && window.crypto?.subtle);
  }

  static supported() {
    return this.canSync() && typeof window.showDirectoryPicker === 'function';
  }

  static allowedEntry(name, directory) {
    return !name.startsWith('.') && !/[\\:\x00-\x1f]/.test(name) &&
      (!directory || !/\.(app|bundle|photoslibrary|framework|pkg)$/i.test(name));
  }

  constructor(request, supportedFile) {
    this.request = request;
    this.supportedFile = supportedFile;
    this.links = new Map();
    this.running = new Map();
    this.states = new Map();
    this.observed = new Map();
    this.failures = new Map();
  }

  key(project, folder) { return project + '/' + folder; }

  database() {
    if (!this.db) this.db = new Promise((resolve,reject) => {
      const opening = indexedDB.open('editroom-device-folders',1);
      opening.onupgradeneeded = () => opening.result.createObjectStore('links');
      opening.onsuccess = () => resolve(opening.result);
      opening.onerror = () => reject(opening.error);
    });
    return this.db;
  }

  async local(key, value) {
    const db = await this.database();
    return new Promise((resolve,reject) => {
      const tx = db.transaction('links',value === undefined ? 'readonly' : 'readwrite');
      const store = tx.objectStore('links');
      const action = value === undefined ? store.get(key) : store.put(value,key);
      tx.oncomplete = () => resolve(action.result);
      tx.onabort = () => reject(tx.error || new Error('The folder link could not be saved.'));
      tx.onerror = () => reject(tx.error);
    });
  }

  async save(project, folder, handle, binding) {
    const key = this.key(project,folder);
    const entry = {handle,binding};
    await this.local(key,entry);
    this.links.set(key,entry);
    this.states.delete(key);
    this.observed.delete(key);
    this.failures.delete(key);
  }

  async link(key) {
    if (!this.links.has(key)) this.links.set(key,await this.local(key));
    return this.links.get(key);
  }

  async copyLinks(from, to, folders) {
    const results = await Promise.allSettled(folders.filter(folder => folder.kind === 'device').map(async folder => {
      const entry = await this.link(this.key(from,folder.id));
      if (entry) await this.save(to,folder.id,entry.handle,entry.binding);
    }));
    return results.some(result => result.status === 'rejected');
  }

  status(project, folder) { return this.states.get(this.key(project,folder)); }

  requestAccess(project, folder) {
    const key = this.key(project,folder);
    const entry = this.links.get(key);
    if (entry && this.states.get(key) === 'permission') {
      return entry.handle.requestPermission({mode:'read'});
    }
  }

  async relations(project, folders, dropped) {
    let best;
    const children = [];
    for (const folder of folders.filter(folder=>folder.kind==='device')) {
      try {
        const entry = await this.link(this.key(project,folder.id));
        if (!entry) continue;
        const parts = await entry.handle.resolve(dropped);
        if (parts && (!best || parts.length<best.parts.length)) best={folder,parts};
        if (!parts) {
          const prefix = await dropped.resolve(entry.handle);
          if (prefix?.length) children.push({folder:folder.id,binding:entry.binding,path:prefix.join('/')});
        }
      } catch (_) {
        // A folder linked on another device may no longer be accessible here.
      }
    }
    return {linked:best ? {folder:best.folder,path:best.parts.join('/')} : null,children};
  }

  async assets(project, folder, path = '') {
    const entry = await this.link(this.key(project,folder));
    if (!entry) return [];
    const endpoint = '/api/projects/' + project + '/device-folders/' + folder;
    const {files} = await this.request(endpoint,{headers:{'x-editroom-folder-binding':entry.binding}});
    return files.filter(file=>!file.offline && DeviceFolders.within(path,file.path))
      .sort((a,b)=>DeviceFolders.comparePaths(a.path,b.path)).map(file=>file.asset_id);
  }

  static within(folder,path) { return !folder || path === folder || path.startsWith(folder + '/'); }
  static comparePaths = new Intl.Collator(undefined,{numeric:true,sensitivity:'base'}).compare;

  sync(project, folder, manual = false, offerMatch = false, scope = null) {
    const key = this.key(project,folder);
    const current = this.running.get(key);
    if (current) return manual ? current.catch(()=>{}).then(()=>this.sync(project,folder,manual,offerMatch,scope)) : current;
    const work = () => this.scan(project,folder,key,manual,offerMatch,scope);
    const task = (navigator.locks
      ? navigator.locks.request('editroom-device-folder/' + key,{ifAvailable:!manual},lock=> lock ? work() : undefined)
      : work()).finally(()=>this.running.delete(key));
    this.running.set(key,task);
    return task;
  }

  async scan(project, folder, key, manual, offerMatch, scope) {
    const endpoint = '/api/projects/' + project + '/device-folders/' + folder;
    try {
      const entry = await this.link(key);
      if (!entry) {
        this.states.set(key,'connect');
        return;
      }
      const options = {mode:'read'};
      const directory = scope?.handle || entry.handle;
      if (await directory.queryPermission(options) !== 'granted') {
        if (!manual || await directory.requestPermission(options) !== 'granted') {
          this.states.set(key,'permission');
          if (scope) throw new DOMException('Restore access to the linked folder to import it.', 'NotAllowedError');
          return;
        }
      }

      const headers = {'x-editroom-folder-binding':entry.binding};
      const saved = await this.request(endpoint,{headers});
      const known = new Map(saved.files.map(file=>[file.path,file]));
      const localFiles = new Map();
      const files = [];
      const previous = this.observed.get(key) || new Map();
      const seen = new Map();
      let skipped = 0;
      if (scope) {
        for (const item of saved.files) {
          if (DeviceFolders.within(scope.path,item.path) || item.offline || !item.stamp) continue;
          files.push({path:item.path,...item.stamp});
          if (previous.has(item.path)) seen.set(item.path,previous.get(item.path));
        }
      }
      for await (const [path,handle] of this.readFolder(directory,scope?.path || '')) {
        if (saved.hidden.some(prefix=>path===prefix || path.startsWith(prefix+'/'))) continue;
        const old = known.get(path)?.stamp;
        try {
          const file = await handle.getFile();
          if (file.size <= 0 || file.size > 256*1024*1024) {
            skipped++;
            if (old) files.push({path,...old});
            continue;
          }
          const signature = file.size + '/' + file.lastModified;
          seen.set(path,signature);
          if (!manual && old && (old.size !== file.size || old.modified !== file.lastModified) && previous.get(path) !== signature) {
            files.push({path,...old});
            continue;
          }
          const readBytes = manual || !old || old.size !== file.size || old.modified !== file.lastModified;
          const digest = readBytes ? await this.digest(file) : old.digest;
          const stamp = {path,size:file.size,modified:file.lastModified,digest};
          files.push(stamp);
          localFiles.set(path,{file,stamp});
        } catch (error) {
          if (error.name === 'NotAllowedError' || error.name === 'SecurityError') throw error;
          skipped++;
          if (old) files.push({path,...old});
        }
      }
      this.observed.set(key,seen);

      const changed = new Set(files.map(file=>file.path));
      const needsScan = manual || files.some(file=>{
        const item = known.get(file.path);
        return !item?.stamp || item.offline || item.stamp.digest!==file.digest ||
          item.stamp.modified!==file.modified || item.stamp.size!==file.size;
      }) || saved.files.some(file=>!file.offline && !changed.has(file.path));
      const failed = manual ? new Map() : this.failures.get(key) || new Map();
      if (!needsScan) {
        this.states.set(key,skipped || failed.size ? 'partial' : 'connected');
        return;
      }
      const plan = await this.request(endpoint,{method:'POST',headers,data:{files,force:manual,revision:saved.revision}});
      let uploaded = 0;
      for (const next of plan.uploads.sort((a,b)=>DeviceFolders.comparePaths(a.path,b.path))) {
        const item = localFiles.get(next.path);
        if (!item || failed.get(next.path) === item.stamp.digest) continue;
        const {file,stamp} = item;
        const params = new URLSearchParams({path:next.path,modified:stamp.modified,digest:stamp.digest});
        if (next.expected) params.set('expected',next.expected);
        if (offerMatch) params.set('offer_match',true);
        try {
          await this.request(endpoint + '/file?' + params,{method:'POST',headers,file});
          uploaded++;
          failed.delete(next.path);
        } catch (error) {
          if (error.status === 401 || error.status === 403 ||
              error.message.includes('Reconnect this device folder')) throw error;
          // A racing edit or local write should be retried with a fresh scan.
          if (!error.message.includes('changed')) failed.set(next.path,stamp.digest);
        }
      }
      for (const path of failed.keys()) if (!changed.has(path)) failed.delete(path);
      this.failures.set(key,failed);
      this.states.set(key,skipped || failed.size ? 'partial' : 'connected');
      return {changed:plan.changed || uploaded>0,uploaded,renamed:plan.renamed,skipped:skipped+failed.size};
    } catch (error) {
      if (error.name === 'NotAllowedError' || error.name === 'SecurityError') this.states.set(key,'permission');
      else if (error.name === 'NotFoundError' || error.message.includes('Reconnect this device folder')) {
        this.states.set(key,'connect');
        this.links.delete(key);
      }
      else this.states.set(key,'error');
      if (manual) throw error;
    }
  }

  async *readFolder(handle, prefix = '') {
    for await (const entry of handle.values()) {
      const name = entry.name;
      if (!DeviceFolders.allowedEntry(name,entry.kind === 'directory')) continue;
      const path = prefix ? prefix + '/' + name : name;
      if (entry.kind === 'directory') {
        yield* this.readFolder(entry,path);
      } else if (this.supportedFile(entry)) {
        yield [path,entry];
      }
    }
  }

  async digest(file) {
    const bytes = await crypto.subtle.digest('SHA-256',await file.arrayBuffer());
    return Array.from(new Uint8Array(bytes),byte=>byte.toString(16).padStart(2,'0')).join('');
  }
}
