use super::*;

#[derive(Clone, Eq, PartialEq, Deserialize, Serialize)]
pub struct DeviceStamp {
    pub size: u64,
    pub modified: i64,
    pub digest: String,
}

impl DeviceStamp {
    pub fn validate(&self, limit: u64) -> Result<()> {
        ensure!(
            self.size > 0 && self.size <= limit,
            "Use a media file up to 256 MB."
        );
        ensure!(
            self.digest.len() == 64
                && self
                    .digest
                    .bytes()
                    .all(|ch| ch.is_ascii_digit() || (b'a'..=b'f').contains(&ch)),
            "A device file needs a SHA-256 digest."
        );
        Ok(())
    }
}

#[derive(Deserialize)]
pub struct DeviceFile {
    pub path: String,
    #[serde(flatten)]
    pub stamp: DeviceStamp,
}

#[derive(Deserialize)]
pub struct DeviceChild {
    pub folder: Uuid,
    pub binding: Uuid,
    pub path: String,
}

impl Library {
    pub fn create_device(&mut self, name: String, children: Vec<DeviceChild>) -> Result<Value> {
        let mut roots = HashMap::new();
        for child in children {
            self.device(child.folder, child.binding)?;
            let prefix = checked_relative(&child.path, false)?;
            ensure!(
                roots.insert(child.folder, prefix).is_none(),
                "A linked folder appears twice."
            );
        }
        let relocate = |place: &EntryPath| {
            let Origin::Upload(id) = place.origin else {
                return None;
            };
            roots.get(&id).map(|prefix| join(prefix, &place.path))
        };
        let mut paths = std::collections::HashSet::new();
        for path in self
            .entries
            .values()
            .filter_map(|entry| relocate(&entry.place))
        {
            let path = checked_relative(&path, false)?;
            ensure!(
                paths.insert(path),
                "Two linked media items have the same path in this folder."
            );
        }
        let mut folder = self.upload_folder(name);
        let binding = Uuid::new_v4();
        folder.device = Some(binding);
        self.folders.last_mut().expect("created folder").device = Some(binding);
        for place in self
            .entries
            .values_mut()
            .map(|entry| &mut entry.place)
            .chain(&mut self.hidden)
        {
            if let Some(path) = relocate(place) {
                place.origin = Origin::Upload(folder.id);
                place.path = path;
            }
        }
        self.folders
            .retain(|folder| !roots.contains_key(&folder.id));
        Ok(json!({"folder":folder.public(),"binding":binding}))
    }

    pub fn bind_device(&mut self, folder: Uuid) -> Result<Value> {
        let folder = self
            .folders
            .iter_mut()
            .find(|item| item.id == folder && item.inbox.is_none())
            .context("Choose an uploaded or device media folder.")?;
        let binding = Uuid::new_v4();
        folder.device = Some(binding);
        Ok(json!({"folder":folder.public(),"binding":binding}))
    }

    fn device(&self, folder: Uuid, binding: Uuid) -> Result<()> {
        ensure!(
            self.folders
                .iter()
                .any(|item| item.id == folder && item.device == Some(binding)),
            "Reconnect this device folder."
        );
        Ok(())
    }

    fn device_hidden(&self, folder: Uuid, path: &str) -> bool {
        self.hidden
            .iter()
            .any(|entry| entry.origin == Origin::Upload(folder) && under(&entry.path, path))
    }

    pub fn device_destination(
        &self,
        folder: Uuid,
        binding: Uuid,
        path: &str,
    ) -> Result<(EntryPath, Option<Uuid>)> {
        self.device(folder, binding)?;
        let path = checked_relative(path, false)?;
        ensure!(
            might_be_media(&path) && !self.device_hidden(folder, &path),
            "That file is outside the selected media folder."
        );
        let place = EntryPath {
            origin: Origin::Upload(folder),
            path,
        };
        let old = self.existing(&place);
        Ok((place, old))
    }

    pub fn set_device_file(&mut self, place: EntryPath, source: Uuid, stamp: DeviceStamp) {
        if !self.entries.contains_key(&source) {
            self.add_upload(place, source);
        }
        let entry = self.entries.get_mut(&source).expect("device entry");
        entry.device_stamp = Some(stamp);
        entry.offline = false;
        entry.refresh_failed = false;
    }
}

pub fn device_snapshot(project: &Project, folder: Uuid, binding: Uuid) -> Result<Value> {
    let library = &project.library;
    library.device(folder, binding)?;
    let mut files = library.entries.iter().filter(|(_,entry)| entry.place.origin == Origin::Upload(folder))
        .map(|(id,entry)| json!({"asset_id":id,"path":entry.place.path,"stamp":entry.device_stamp,"offline":entry.offline}))
        .collect::<Vec<_>>();
    files.sort_by(|a, b| a["path"].as_str().cmp(&b["path"].as_str()));
    let hidden = library
        .hidden
        .iter()
        .filter(|entry| entry.origin == Origin::Upload(folder))
        .map(|entry| &entry.path)
        .collect::<Vec<_>>();
    Ok(json!({"files":files,"hidden":hidden,"revision":project.library_revision}))
}

pub fn device_manifest(
    project: &mut Project,
    folder: Uuid,
    binding: Uuid,
    files: Vec<DeviceFile>,
    force: bool,
    limit: u64,
) -> Result<Value> {
    project.library.device(folder, binding)?;
    ensure!(
        files.len() <= MAX_ENTRIES,
        "This device folder is too large to scan."
    );
    let mut manifest = BTreeMap::new();
    for file in files {
        let path = checked_relative(&file.path, false)?;
        if project.library.device_hidden(folder, &path) || !might_be_media(&path) {
            continue;
        }
        file.stamp.validate(limit)?;
        ensure!(
            manifest.insert(path, file.stamp).is_none(),
            "A file appears twice in the device scan."
        );
    }
    let entries = project
        .library
        .entries
        .iter_mut()
        .filter(|(_, entry)| entry.place.origin == Origin::Upload(folder))
        .collect::<Vec<_>>();
    let mut known = entries
        .iter()
        .map(|(id, entry)| (entry.place.path.clone(), **id))
        .collect::<HashMap<_, _>>();
    let mut missing = HashMap::<String, Vec<Uuid>>::new();
    for (id, entry) in &entries {
        if !manifest.contains_key(&entry.place.path)
            && let Some(stamp) = &entry.device_stamp
        {
            missing.entry(stamp.digest.clone()).or_default().push(**id);
        }
    }
    let mut additions = HashMap::<&str, Vec<&str>>::new();
    for (path, stamp) in &manifest {
        if !known.contains_key(path) {
            additions.entry(&stamp.digest).or_default().push(path);
        }
    }
    let mut renamed = 0;
    let mut changed = false;
    let mut uploads = vec![];
    for (id, entry) in entries {
        // A unique exact digest on both sides can identify a rename. Duplicates
        // remain distinct so the wrong take cannot be substituted into the cut.
        if !manifest.contains_key(&entry.place.path)
            && let Some(stamp) = &entry.device_stamp
            && let Some(ids) = missing.get(&stamp.digest)
            && ids.len() == 1
            && let Some(paths) = additions.get(stamp.digest.as_str())
            && paths.len() == 1
        {
            let previous = std::mem::replace(&mut entry.place.path, paths[0].into());
            known.remove(&previous);
            known.insert(entry.place.path.clone(), *id);
            if let Some(source) = project.sources.iter_mut().find(|source| source.id == *id)
                && source.title == default_title(&previous)
            {
                source.title = default_title(&entry.place.path);
            }
            changed = true;
            renamed += 1;
        }
        let current = manifest.get(&entry.place.path);
        let offline = current.is_none();
        changed |= entry.offline != offline;
        entry.offline = offline;
        let Some(current) = current else {
            continue;
        };
        let Some(source) = project.sources.iter().find(|source| source.id == *id) else {
            continue;
        };
        let installed = if force {
            source.device_digest.as_deref()
        } else {
            entry
                .device_stamp
                .as_ref()
                .map(|stamp| stamp.digest.as_str())
        };
        if installed != Some(current.digest.as_str()) {
            uploads.push(json!({"path":entry.place.path,"expected":source.storage_id()}));
        } else if entry.device_stamp.as_ref() != Some(current) {
            entry.device_stamp = Some(current.clone());
            changed = true;
        }
    }
    uploads.extend(
        manifest
            .keys()
            .filter(|path| !known.contains_key(*path))
            .map(|path| json!({"path":path})),
    );
    if changed {
        project.library_revision += 1;
    }
    Ok(json!({"uploads":uploads,"renamed":renamed,"changed":changed}))
}
