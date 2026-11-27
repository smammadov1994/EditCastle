mod device;
pub use device::{DeviceChild, DeviceFile, DeviceStamp, device_manifest, device_snapshot};

use crate::{
    editor::{LibraryChange, LibrarySnapshot, Project, Source, Store},
    media,
};
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    ffi::{CStr, CString},
    fs::{self, File, OpenOptions},
    io,
    os::{
        fd::{AsRawFd, FromRawFd, IntoRawFd},
        unix::fs::{MetadataExt, OpenOptionsExt},
    },
    path::{Component, Path, PathBuf},
};
use uuid::Uuid;

const MAX_ENTRIES: usize = 20_000;
pub(crate) const IMPORT_BATCH: usize = 60;

#[derive(Clone, Default, Eq, PartialEq, Deserialize, Serialize)]
pub struct Library {
    pub folders: Vec<Folder>,
    pub entries: HashMap<Uuid, Entry>,
    pub hidden: Vec<EntryPath>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub missing: HashMap<Uuid, String>,
}

impl Library {
    pub fn is_empty(&self) -> bool {
        self.folders.is_empty()
            && self.entries.is_empty()
            && self.hidden.is_empty()
            && self.missing.is_empty()
    }

    pub fn restore(&mut self, from: &Self, to: &Self) {
        self.folders.retain(|folder| {
            !from.folders.iter().any(|prior| prior.id == folder.id)
                || to.folders.iter().any(|target| target.id == folder.id)
        });
        for target in &to.folders {
            if from.folders.iter().any(|folder| folder == target) {
                continue;
            }
            if let Some(folder) = self
                .folders
                .iter_mut()
                .find(|folder| folder.id == target.id)
            {
                *folder = target.clone();
            } else {
                self.folders.push(target.clone());
            }
        }
        self.entries
            .retain(|id, _| !from.entries.contains_key(id) || to.entries.contains_key(id));
        self.entries.extend(
            to.entries
                .iter()
                .filter(|(id, entry)| from.entries.get(id) != Some(*entry))
                .map(|(id, entry)| (*id, entry.clone())),
        );
        self.missing
            .retain(|id, _| !from.missing.contains_key(id) || to.missing.contains_key(id));
        self.missing.extend(
            to.missing
                .iter()
                .filter(|(id, name)| from.missing.get(id) != Some(*name))
                .map(|(id, name)| (*id, name.clone())),
        );
        self.hidden
            .retain(|path| !from.hidden.contains(path) || to.hidden.contains(path));
        self.hidden.extend(
            to.hidden
                .iter()
                .filter(|path| !from.hidden.contains(path))
                .cloned(),
        );
    }

    pub fn can_reload(&self, id: Uuid) -> bool {
        self.entries
            .get(&id)
            .is_some_and(|entry| entry.place.origin == Origin::Inbox)
    }

    pub fn offline(&self, id: Uuid) -> bool {
        self.missing.contains_key(&id) || self.entries.get(&id).is_some_and(|entry| entry.offline)
    }

    pub fn watches_inbox(&self) -> bool {
        self.folders.iter().any(|folder| folder.inbox.is_some())
            || self
                .entries
                .values()
                .any(|entry| entry.place.origin == Origin::Inbox)
    }

    pub fn locate(&self, id: Uuid) -> Option<(&Folder, &str)> {
        self.locate_path(&self.entries.get(&id)?.place)
    }

    pub fn excluded_paths(&self) -> impl Iterator<Item = (&Folder, &str)> {
        self.hidden
            .iter()
            .filter_map(|place| self.locate_path(place))
    }

    fn locate_path<'a>(&'a self, place: &'a EntryPath) -> Option<(&'a Folder, &'a str)> {
        match place.origin {
            Origin::Inbox => {
                let folder = self.folders.iter().find(|folder| {
                    folder
                        .inbox
                        .as_deref()
                        .is_some_and(|root| under(root, &place.path))
                })?;
                let root = folder.inbox.as_ref().unwrap();
                let relative = place.path.strip_prefix(root)?.trim_start_matches('/');
                Some((folder, relative))
            }
            Origin::Upload(id) => Some((
                self.folders.iter().find(|folder| folder.id == id)?,
                &place.path,
            )),
        }
    }

    pub fn display_folder(&self, id: Uuid) -> Option<String> {
        let (folder, path) = self.locate(id)?;
        let parent = path
            .rsplit_once('/')
            .map(|(parent, _)| parent)
            .unwrap_or("");
        if parent.is_empty() {
            Some(folder.name.clone())
        } else {
            Some(format!("{} / {}", folder.name, parent.replace('/', " / ")))
        }
    }

    pub fn source_json(&self, source: &Source) -> Value {
        let mut result = serde_json::to_value(source).expect("source is serializable");
        let values = result.as_object_mut().expect("source fields");
        values.remove("origin_stamp");
        values.remove("device_digest");
        values.remove("storage");
        values.insert("version".into(), json!(source.storage_id()));
        if let Some((folder, path)) = self.locate(source.id) {
            result["folder_id"] = json!(folder.id);
            result["relative_path"] = json!(path);
        }
        if let Some(entry) = self.entries.get(&source.id) {
            result["offline"] = json!(entry.offline);
            result["reloadable"] = json!(entry.place.origin == Origin::Inbox);
            if entry.refresh_failed {
                result["refresh_failed"] = json!(true);
            }
        }
        if let Some(name) = self.missing.get(&source.id) {
            result["offline"] = json!(true);
            result["missing_file"] = json!(name);
        }
        result
    }

    pub fn scope(&self, id: Uuid, folder: Uuid, subpath: &str) -> bool {
        self.locate(id)
            .is_some_and(|(root, path)| root.id == folder && under(subpath, path))
    }

    pub fn upload_folder(&mut self, name: String) -> Folder {
        let folder = Folder {
            id: Uuid::new_v4(),
            name,
            inbox: None,
            identity: None,
            device: None,
            offline: false,
        };
        self.folders.push(folder.clone());
        folder
    }

    pub fn upload_path(&self, folder: Uuid, path: &str) -> Result<EntryPath> {
        ensure!(
            self.folders
                .iter()
                .any(|item| item.id == folder && item.inbox.is_none() && item.device.is_none()),
            "Upload folder not found."
        );
        let path = checked_relative(path, false)?;
        Ok(EntryPath {
            origin: Origin::Upload(folder),
            path,
        })
    }

    pub fn existing(&self, place: &EntryPath) -> Option<Uuid> {
        self.entries
            .iter()
            .find(|(_, entry)| &entry.place == place)
            .map(|(id, _)| *id)
    }

    pub fn add_upload(&mut self, place: EntryPath, id: Uuid) {
        self.entries.insert(
            id,
            Entry {
                place,
                identity: None,
                offline: false,
                observed: None,
                device_stamp: None,
                refresh_failed: false,
            },
        );
    }
}

#[derive(Clone, Eq, PartialEq, Deserialize, Serialize)]
pub struct Folder {
    pub id: Uuid,
    pub name: String,
    pub inbox: Option<String>,
    identity: Option<Identity>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    device: Option<Uuid>,
    pub offline: bool,
}

impl Folder {
    pub fn public(&self) -> Value {
        json!({"id":self.id,"name":self.name,"kind":if self.inbox.is_some() {"inbox"} else if self.device.is_some() {"device"} else {"upload"},"offline":self.offline})
    }
}

#[derive(Clone, Copy, Eq, PartialEq, Hash, Deserialize, Serialize)]
pub struct Identity {
    device: u64,
    inode: u64,
}

impl Identity {
    fn at(file: &File) -> Result<Self> {
        let info = file.metadata()?;
        Ok(Self {
            device: info.dev(),
            inode: info.ino(),
        })
    }
}

#[derive(Clone, Copy, Eq, Hash, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Origin {
    Inbox,
    Upload(Uuid),
}

#[derive(Clone, Eq, Hash, PartialEq, Deserialize, Serialize)]
pub struct EntryPath {
    origin: Origin,
    path: String,
}

#[derive(Clone, Eq, PartialEq, Deserialize, Serialize)]
pub struct Entry {
    place: EntryPath,
    identity: Option<Identity>,
    offline: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    observed: Option<FileStamp>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    device_stamp: Option<DeviceStamp>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    refresh_failed: bool,
}

#[derive(Clone, Copy, Eq, PartialEq, Deserialize, Serialize)]
pub struct FileStamp {
    identity: Identity,
    len: u64,
    modified: i64,
    nanos: i64,
}

impl FileStamp {
    fn read(meta: &fs::Metadata) -> Self {
        Self {
            identity: Identity {
                device: meta.dev(),
                inode: meta.ino(),
            },
            len: meta.len(),
            modified: meta.mtime(),
            nanos: meta.mtime_nsec(),
        }
    }
}

#[derive(Default)]
pub struct InboxState {
    failed: HashMap<(Uuid, String), FileStamp>,
    settling: HashMap<(Uuid, String), FileStamp>,
}

impl InboxState {
    pub fn retry(&mut self, project: Uuid) {
        self.failed.retain(|(id, _), _| *id != project);
    }
}

impl EntryPath {
    pub fn title(&self) -> String {
        default_title(&self.path)
    }
}

fn under(root: &str, path: &str) -> bool {
    root.is_empty()
        || root == path
        || path
            .strip_prefix(root)
            .is_some_and(|rest| rest.starts_with('/'))
}

fn join(parent: &str, name: &str) -> String {
    if parent.is_empty() {
        name.into()
    } else {
        format!("{parent}/{name}")
    }
}

fn name_of(path: &str) -> &str {
    path.rsplit('/')
        .next()
        .filter(|name| !name.is_empty())
        .unwrap_or("Inbox")
}

fn default_title(path: &str) -> String {
    Path::new(path)
        .file_stem()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or("Media")
        .chars()
        .take(100)
        .collect()
}

pub fn checked_relative(raw: &str, empty: bool) -> Result<String> {
    if empty && raw.is_empty() {
        return Ok(String::new());
    }
    ensure!(
        raw.len() <= 3000 && !raw.chars().any(char::is_control),
        "Choose a path inside the studio media inbox."
    );
    let path = Path::new(raw);
    let names = path
        .components()
        .map(|part| match part {
            Component::Normal(name) => name
                .to_str()
                .filter(|name| {
                    !name.starts_with('.') && !name.contains('\\') && !name.contains(':')
                })
                .context("Hidden and invalid paths are unavailable."),
            _ => bail!("Choose a path inside the studio media inbox."),
        })
        .collect::<Result<Vec<_>>>()?;
    ensure!(!names.is_empty(), "Choose a file or folder.");
    Ok(names.join("/"))
}

fn open_component(parent: &File, name: &str, directory: bool) -> io::Result<File> {
    let name = CString::new(name)?;
    let mut flags = libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK;
    if directory {
        flags |= libc::O_DIRECTORY;
    }
    // The parent descriptor is already anchored in the inbox. Each next component forbids symlinks.
    let fd = unsafe { libc::openat(parent.as_raw_fd(), name.as_ptr(), flags) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { File::from_raw_fd(fd) })
}

fn children(parent: &File) -> Result<Vec<String>> {
    struct Directory(*mut libc::DIR);
    impl Drop for Directory {
        fn drop(&mut self) {
            unsafe {
                libc::closedir(self.0);
            }
        }
    }
    let fd = parent.try_clone()?.into_raw_fd();
    // fdopendir owns the duplicate on success. readdir's borrowed name is copied before advancing.
    let pointer = unsafe { libc::fdopendir(fd) };
    if pointer.is_null() {
        let error = io::Error::last_os_error();
        unsafe {
            libc::close(fd);
        }
        return Err(error.into());
    }
    let dir = Directory(pointer);
    let mut names = vec![];
    loop {
        let next = unsafe { libc::readdir(dir.0) };
        if next.is_null() {
            break;
        }
        let name = unsafe { CStr::from_ptr((*next).d_name.as_ptr()) };
        if let Ok(name) = name.to_str()
            && !name.starts_with('.')
            && !name.chars().any(char::is_control)
            && !name.contains(['\\', ':'])
        {
            names.push(name.into());
        }
    }
    names.sort_unstable();
    Ok(names)
}

fn might_be_media(path: &str) -> bool {
    let ext = Path::new(path)
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    [
        "mp4", "mov", "mkv", "m4a", "mp3", "aac", "wav", "webm", "flac", "jpg", "jpeg", "png",
        "webp", "avi", "mxf", "aiff", "aif", "caf", "ogg", "opus", "ogv", "3gp", "3g2", "mpg",
        "mpeg", "hevc", "h264", "wmv", "ac3", "flv", "mts", "m2ts", "ts", "vob", "f4v",
    ]
    .contains(&ext.as_str())
}

fn package(path: &str) -> bool {
    let name = path.to_ascii_lowercase();
    [".app", ".bundle", ".photoslibrary", ".framework", ".pkg"]
        .iter()
        .any(|suffix| name.ends_with(suffix))
}

pub fn inbox_path(store: &Store) -> PathBuf {
    (!store.account)
        .then(|| std::env::var_os("EDITROOM_MEDIA_INBOX"))
        .flatten()
        .map(PathBuf::from)
        .unwrap_or_else(|| store.root.join("inbox"))
}

pub struct Inventory {
    directory: File,
    base: PathBuf,
    folders: BTreeMap<String, Identity>,
    files: BTreeMap<String, FileStamp>,
}

impl Inventory {
    fn scan(path: &Path) -> Result<Self> {
        fs::create_dir_all(path).context("Open the studio media inbox.")?;
        Self::scan_existing(path)
    }

    pub(crate) fn scan_existing(path: &Path) -> Result<Self> {
        let base = fs::canonicalize(path).context("Open the studio media inbox.")?;
        let directory = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
            .open(&base)
            .context("Open the studio media inbox.")?;
        let mut folders = BTreeMap::new();
        let mut files = BTreeMap::new();
        folders.insert(String::new(), Identity::at(&directory)?);
        let mut stack = vec![String::new()];
        let mut seen = 0;
        while let Some(parent) = stack.pop() {
            let dir = match Self::open(&directory, &parent, true) {
                Ok(dir) => dir,
                Err(_) => continue,
            };
            for name in children(&dir)? {
                seen += 1;
                ensure!(
                    seen <= MAX_ENTRIES,
                    "The studio media inbox is too large to scan."
                );
                let path = join(&parent, &name);
                if let Ok(child) = open_component(&dir, &name, true) {
                    if !package(&name) {
                        folders.insert(path.clone(), Identity::at(&child)?);
                        stack.push(path);
                    }
                } else if might_be_media(&path)
                    && let Ok(child) = open_component(&dir, &name, false)
                    && child.metadata().is_ok_and(|metadata| metadata.is_file())
                {
                    files.insert(path, FileStamp::read(&child.metadata()?));
                }
            }
        }
        Ok(Self {
            directory,
            base,
            folders,
            files,
        })
    }

    fn open(directory: &File, path: &str, is_dir: bool) -> Result<File> {
        let mut parent = directory.try_clone()?;
        let mut components = path.split('/').filter(|name| !name.is_empty()).peekable();
        while let Some(name) = components.next() {
            parent = open_component(&parent, name, is_dir || components.peek().is_some())?;
        }
        Ok(parent)
    }

    pub fn requested(&self, path: &str) -> Result<String> {
        let path = if Path::new(path).is_absolute() {
            Path::new(path)
                .strip_prefix(&self.base)
                .ok()
                .and_then(Path::to_str)
                .context("Imports are limited to the studio media inbox.")?
        } else {
            path
        };
        checked_relative(path, true)
    }

    pub(crate) fn open_file(&self, path: &str) -> Result<File> {
        let expected = self
            .files
            .get(path)
            .context("Media is unavailable in the studio inbox.")?;
        let file =
            Self::open(&self.directory, path, false).context("Open that studio media file.")?;
        ensure!(
            file.metadata()?.is_file() && FileStamp::read(&file.metadata()?) == *expected,
            "Media changed during import."
        );
        Ok(file)
    }

    pub fn tree(&self) -> Vec<Value> {
        self.folders
            .keys()
            .map(|path| json!({"path":path,"name":name_of(path)}))
            .collect()
    }

    pub fn file_paths(&self) -> impl Iterator<Item = &str> {
        self.files.keys().map(String::as_str)
    }

    pub(crate) fn contains_folder(&self, path: &str) -> bool {
        self.folders.contains_key(path)
    }

    fn only_path<T>(paths: &BTreeMap<String, T>, matches: impl Fn(&T) -> bool) -> Option<&str> {
        let mut found = paths.iter().filter(|(_, value)| matches(value));
        let (path, _) = found.next()?;
        if found.next().is_some() {
            None
        } else {
            Some(path)
        }
    }
}

pub async fn scan(store: &Store) -> Result<Inventory> {
    let path = inbox_path(store);
    tokio::task::spawn_blocking(move || Inventory::scan(&path)).await?
}

fn move_prefix(from: &str, to: &str, path: &str) -> String {
    let relative = path
        .strip_prefix(from)
        .unwrap_or(path)
        .trim_start_matches('/');
    if relative.is_empty() {
        to.into()
    } else {
        join(to, relative)
    }
}

fn move_folder(
    library: &mut Library,
    folder_id: Uuid,
    to: &str,
    inventory: &Inventory,
) -> Result<()> {
    let mut roots = library.folders.iter();
    let root = roots
        .find(|folder| folder.id == folder_id && folder.inbox.is_some())
        .context("Linked folder not found.")?;
    let from = root.inbox.clone().unwrap();
    let identity = inventory
        .folders
        .get(to)
        .context("The chosen folder is unavailable.")?;
    ensure!(
        !library.folders.iter().any(|folder| folder.id != folder_id
            && folder
                .inbox
                .as_deref()
                .is_some_and(|path| under(path, to) || under(to, path))),
        "That location overlaps another linked inbox folder."
    );
    for entry in library.entries.values_mut() {
        if entry.place.origin == Origin::Inbox && under(&from, &entry.place.path) {
            entry.place.path = move_prefix(&from, to, &entry.place.path);
        }
    }
    for hidden in &mut library.hidden {
        if hidden.origin == Origin::Inbox && under(&from, &hidden.path) {
            hidden.path = move_prefix(&from, to, &hidden.path);
        }
    }
    let folder = library
        .folders
        .iter_mut()
        .find(|folder| folder.id == folder_id)
        .unwrap();
    folder.inbox = Some(to.into());
    folder.identity = Some(*identity);
    folder.name = name_of(to).into();
    folder.offline = false;
    Ok(())
}

fn register(library: &mut Library, path: &str, inventory: &Inventory) -> Result<Uuid> {
    let identity = *inventory
        .folders
        .get(path)
        .context("Inbox folder not found.")?;
    library.hidden.retain(|hidden| {
        hidden.origin != Origin::Inbox || !(under(path, &hidden.path) || under(&hidden.path, path))
    });
    if let Some(folder) = library.folders.iter().find(|folder| {
        folder
            .inbox
            .as_deref()
            .is_some_and(|root| under(root, path))
    }) {
        return Ok(folder.id);
    }
    library.folders.retain(|folder| {
        !folder
            .inbox
            .as_deref()
            .is_some_and(|root| under(path, root))
    });
    let id = Uuid::new_v4();
    library.folders.push(Folder {
        id,
        name: name_of(path).into(),
        inbox: Some(path.into()),
        device: None,
        identity: Some(identity),
        offline: false,
    });
    Ok(id)
}

fn reconcile(library: &mut Library, inventory: &Inventory) -> Vec<(Uuid, String)> {
    // Resolve directory moves before files so inferred nested folders stay in place.
    let moves = library
        .folders
        .iter()
        .filter_map(|folder| {
            let old = folder.inbox.as_deref()?;
            if inventory.folders.contains_key(old) {
                return None;
            }
            Inventory::only_path(&inventory.folders, |id| Some(*id) == folder.identity)
                .map(|path| (folder.id, path.to_owned()))
        })
        .collect::<Vec<_>>();
    for (id, to) in moves {
        let _ = move_folder(library, id, &to, inventory);
    }
    for folder in &mut library.folders {
        if let Some(path) = folder.inbox.as_deref() {
            folder.offline = !inventory.folders.contains_key(path);
        }
    }
    let mut renames = vec![];
    for (id, entry) in &mut library.entries {
        if entry.place.origin != Origin::Inbox {
            continue;
        }
        // Prefer the same inode when a file moved. Keep the private copy usable if it left the inbox.
        let old = entry.place.path.clone();
        let at_path = inventory.files.get(&old).map(|stamp| stamp.identity);
        let previous = entry.identity;
        if at_path != previous
            && let Some(previous) = previous
            && let Some(path) =
                Inventory::only_path(&inventory.files, |stamp| stamp.identity == previous)
            && path != old
        {
            entry.place.path = path.into();
            renames.push((*id, old));
        }
        entry.offline = !inventory.files.contains_key(&entry.place.path);
        if !entry.offline {
            entry.identity = inventory
                .files
                .get(&entry.place.path)
                .map(|stamp| stamp.identity);
        }
    }
    renames
}

async fn ingest(store: &Store, inventory: &Inventory, path: &str, limit: usize) -> Result<Source> {
    let mut input = tokio::fs::File::from_std(inventory.open_file(path)?);
    let stamp = FileStamp::read(&input.metadata().await?);
    let size = stamp.len;
    ensure!(
        size > 0 && size <= limit as u64,
        "Use a nonempty media file up to 256 MB."
    );
    let stage = media::temp(store).await?;
    let result = async {
        use tokio::io::AsyncReadExt;
        let path_on_host = stage.join("source.media");
        let mut staged = tokio::fs::File::create(&path_on_host).await?;
        let copied = tokio::io::copy(&mut (&mut input).take(limit as u64 + 1), &mut staged).await?;
        ensure!(
            copied == size && FileStamp::read(&input.metadata().await?) == stamp,
            "Media changed during import."
        );
        drop(staged);
        let mut source = media::import(store, &path_on_host, default_title(path)).await?;
        source.origin_stamp = Some(stamp);
        Ok(source)
    }
    .await;
    let _ = tokio::fs::remove_dir_all(stage).await;
    result
}

pub async fn locate_source(
    store: &Store,
    id: Uuid,
    inventory: &Inventory,
    asset: Uuid,
    path: &str,
    limit: usize,
) -> Result<Value> {
    let path = inventory.requested(path)?;
    let stamp = *inventory
        .files
        .get(&path)
        .context("Choose a media file from the studio inbox.")?;
    let (candidates, revision) = store.read(|db| {
        let project = db
            .projects
            .iter()
            .find(|project| project.id == id)
            .context("Project not found.")?;
        let entry = project
            .library
            .entries
            .get(&asset)
            .filter(|entry| entry.place.origin == Origin::Inbox)
            .context("That source is not linked to the studio inbox.")?;
        ensure!(
            !inventory.files.contains_key(&entry.place.path),
            "This source is available. Use Reload from Disk to refresh it."
        );
        let assigned = project
            .library
            .entries
            .iter()
            .filter(|&(&other, entry)| other != asset && entry.place.origin == Origin::Inbox)
            .map(|(_, entry)| entry.place.path.as_str())
            .collect::<HashSet<_>>();
        ensure!(
            !assigned.contains(path.as_str()),
            "That file already belongs to another source in this project."
        );
        let old = project
            .source(asset)
            .cloned()
            .context("Source not found in this project.")?;
        let mut candidates = vec![(old, entry.clone(), path.clone(), stamp)];
        let parent = path.rsplit_once('/').map_or("", |(parent, _)| parent);
        let mut siblings = BTreeMap::new();
        for source in project.sources.iter().filter(|source| source.id != asset) {
            let Some(prior) = project.library.entries.get(&source.id).filter(|entry| {
                entry.place.origin == Origin::Inbox
                    && !inventory.files.contains_key(&entry.place.path)
            }) else {
                continue;
            };
            let candidate = join(parent, name_of(&prior.place.path));
            if candidate == path || assigned.contains(candidate.as_str()) {
                continue;
            }
            let Some(&stamp) = inventory.files.get(&candidate) else {
                continue;
            };
            siblings
                .entry(candidate)
                .and_modify(|item| *item = None)
                .or_insert_with(|| Some((source.clone(), prior.clone(), stamp)));
        }
        candidates.extend(siblings.into_iter().filter_map(|(path, item)| {
            let (source, entry, stamp) = item?;
            Some((source, entry, path, stamp))
        }));
        Ok::<_, anyhow::Error>((candidates, project.library_revision))
    })?;
    let mut replacements = vec![];
    let mut entries = vec![];
    for (old, mut entry, path, stamp) in candidates {
        let loaded = match ingest(store, inventory, &path, limit).await {
            Ok(loaded) if old.same_kind(&loaded) => loaded,
            Ok(loaded) => {
                media::discard_import(store, &loaded).await;
                ensure!(
                    old.id != asset,
                    "Choose the same media kind for this source."
                );
                continue;
            }
            Err(error) if old.id == asset => return Err(error),
            Err(_) => continue,
        };
        entry.place.path = path;
        entry.identity = Some(stamp.identity);
        entry.observed = Some(stamp);
        entry.offline = false;
        entry.refresh_failed = false;
        replacements.push((old.id, loaded));
        entries.push((old.id, entry));
    }
    let located = entries.iter().map(|(id, _)| *id).collect::<Vec<_>>();
    let result = store.update(|db| {
        let project = db
            .projects
            .iter_mut()
            .find(|project| project.id == id)
            .context("Project not found.")?;
        ensure!(
            project.library_revision == revision,
            "The media library changed during this operation."
        );
        project.replace_sources_and_layout(replacements.clone(), |library| {
            library.entries.extend(entries);
        });
        Ok(json!({"located":located}))
    });
    if result.is_err() {
        for (_, source) in replacements {
            media::discard_import(store, &source).await;
        }
    }
    result
}

pub enum Action {
    Import {
        paths: Vec<String>,
        offer_match: bool,
    },
    Sync,
    Discover,
    Locate {
        folder: Uuid,
        path: String,
    },
    Reload(ReloadTarget),
}

pub enum ReloadTarget {
    All,
    Folder(Uuid, String),
    Asset(Uuid),
    Assets(Vec<Uuid>),
}

pub async fn apply(
    store: &Store,
    project_id: Uuid,
    inventory: &Inventory,
    action: Action,
    limit: usize,
    state: &mut InboxState,
) -> Result<Value> {
    let (before, existing, revision, unlocked_format) = store
        .read(|db| {
            db.projects.iter().find(|p| p.id == project_id).map(|p| {
                (
                    p.library.clone(),
                    p.sources.clone(),
                    p.library_revision,
                    p.sequence.offers_source_format(),
                )
            })
        })
        .context("Project not found.")?;
    let mut next = before.clone();
    let mut scopes = vec![];
    let mut opened = None;
    let import_existing = matches!(&action, Action::Import { .. });
    let offer_match = unlocked_format
        && matches!(
            &action,
            Action::Import {
                offer_match: true,
                ..
            }
        );
    let auto_scope = matches!(&action, Action::Sync | Action::Discover);
    let discovery = matches!(&action, Action::Discover);
    let mut reload = None;
    let mut loose = vec![];
    match action {
        Action::Import { paths, .. } => {
            for path in paths {
                let path = inventory.requested(&path)?;
                if inventory.folders.contains_key(&path) {
                    let id = register(&mut next, &path, inventory)?;
                    let root = next
                        .folders
                        .iter()
                        .find(|folder| folder.id == id)
                        .and_then(|folder| folder.inbox.as_deref())
                        .unwrap_or("");
                    let relative = path
                        .strip_prefix(root)
                        .unwrap_or("")
                        .trim_start_matches('/');
                    opened.get_or_insert(json!({"id":id,"path":relative}));
                    scopes.push(path);
                } else if inventory.files.contains_key(&path) {
                    next.hidden.retain(|hidden| {
                        hidden.origin != Origin::Inbox || !under(&hidden.path, &path)
                    });
                    loose.push(path);
                } else {
                    bail!("Media was not found in the studio inbox.");
                }
            }
        }
        Action::Sync | Action::Discover => {}
        Action::Locate { folder, path } => {
            let path = inventory.requested(&path)?;
            move_folder(&mut next, folder, &path, inventory)?;
            opened = Some(json!({"id":folder,"path":""}));
            reload = Some(ReloadTarget::Folder(folder, String::new()));
        }
        Action::Reload(target) => {
            reload = Some(target);
        }
    }
    let renames = reconcile(&mut next, inventory);
    let all_linked = matches!(reload, Some(ReloadTarget::All));
    if auto_scope || all_linked {
        scopes = next
            .folders
            .iter()
            .filter_map(|folder| folder.inbox.clone())
            .collect();
    }
    match &reload {
        Some(ReloadTarget::Folder(folder, path)) => {
            let path = checked_relative(path, true)?;
            let root = next
                .folders
                .iter()
                .find(|item| item.id == *folder)
                .and_then(|item| item.inbox.as_deref())
                .context("Choose a linked studio inbox folder.")?;
            scopes.push(if path.is_empty() {
                root.into()
            } else {
                join(root, &path)
            });
        }
        Some(ReloadTarget::Asset(asset)) => {
            let entry = next
                .entries
                .get(asset)
                .filter(|entry| entry.place.origin == Origin::Inbox)
                .context("That source is not linked to the studio inbox.")?;
            ensure!(!entry.offline, "The linked source is offline.");
            loose.push(entry.place.path.clone());
        }
        Some(ReloadTarget::Assets(assets)) => {
            ensure!(
                (1..=500).contains(&assets.len()),
                "Select up to 500 inbox sources to reload."
            );
            for asset in assets {
                if let Some(entry) = next
                    .entries
                    .get(asset)
                    .filter(|entry| entry.place.origin == Origin::Inbox && !entry.offline)
                {
                    loose.push(entry.place.path.clone());
                }
            }
            ensure!(
                !loose.is_empty(),
                "The selected inbox sources are offline or unavailable."
            );
        }
        _ => {}
    }
    let batch = if matches!(reload, Some(ReloadTarget::Assets(_))) {
        loose.len()
    } else {
        IMPORT_BATCH
    };
    let known = next
        .entries
        .iter()
        .filter(|(_, entry)| entry.place.origin == Origin::Inbox)
        .map(|(id, entry)| (entry.place.path.clone(), *id))
        .collect::<HashMap<_, _>>();
    let existing = existing
        .into_iter()
        .map(|source| (source.id, source))
        .collect::<HashMap<_, _>>();
    let mut selected: Vec<_> = inventory
        .files
        .iter()
        .filter(|(path, _)| {
            loose.contains(path)
                || (auto_scope || all_linked) && known.contains_key(*path)
                || scopes.iter().any(|scope| under(scope, path))
                    && !next
                        .hidden
                        .iter()
                        .any(|hidden| hidden.origin == Origin::Inbox && under(&hidden.path, path))
        })
        .collect();
    selected.sort_by_key(|(path, _)| (!loose.contains(path), *path));
    let mut imported = vec![];
    let mut added = vec![];
    let mut replacements = vec![];
    let mut skipped = vec![];
    let mut attempts = 0;
    let mut remaining = false;
    for (path, &stamp) in selected {
        let prior = known.get(path).and_then(|id| existing.get(id));
        let retry = (project_id, path.clone());
        if let Some(source) = prior {
            let entry = next.entries.get_mut(&source.id).expect("known inbox entry");
            if import_existing {
                imported.push(source.id);
                continue;
            }
            if discovery {
                continue;
            }
            if reload.is_some() {
                if source.origin_stamp == Some(stamp) && !entry.refresh_failed {
                    continue;
                }
            } else {
                // Entries from older builds have no recorded disk timestamp until this first scan.
                let Some(observed) = entry.observed else {
                    entry.observed = Some(stamp);
                    continue;
                };
                if observed == stamp {
                    continue;
                }
                // Native Editroom waits until a source stops changing before it reloads.
                if state.settling.insert(retry.clone(), stamp) != Some(stamp) {
                    continue;
                }
            }
        }
        if state.failed.get(&retry) == Some(&stamp) {
            continue;
        }
        if attempts >= batch {
            remaining = true;
            continue;
        }
        attempts += 1;
        let loaded = ingest(store, inventory, path, limit).await;
        let loaded = match loaded {
            Ok(source) if prior.is_some_and(|old| !old.same_kind(&source)) => {
                media::discard_import(store, &source).await;
                Err(anyhow::anyhow!("The media kind changed."))
            }
            other => other,
        };
        match loaded {
            Ok(source) => {
                state.failed.remove(&retry);
                state.settling.remove(&retry);
                if let Some(prior) = prior {
                    let entry = next.entries.get_mut(&prior.id).unwrap();
                    entry.observed = Some(stamp);
                    entry.refresh_failed = false;
                    replacements.push((prior.id, source));
                } else {
                    next.entries.insert(
                        source.id,
                        Entry {
                            place: EntryPath {
                                origin: Origin::Inbox,
                                path: path.clone(),
                            },
                            identity: Some(stamp.identity),
                            offline: false,
                            observed: Some(stamp),
                            device_stamp: None,
                            refresh_failed: false,
                        },
                    );
                    imported.push(source.id);
                    added.push(source);
                }
            }
            Err(_) => {
                state.failed.insert(retry, stamp);
                if let Some(prior) = prior {
                    next.entries.get_mut(&prior.id).unwrap().refresh_failed = true;
                }
                skipped.push(name_of(path).to_owned());
            }
        }
    }
    state
        .failed
        .retain(|(id, path), _| *id != project_id || inventory.files.contains_key(path));
    state.settling.retain(|(id, path), _| {
        *id != project_id || known.contains_key(path) && inventory.files.contains_key(path)
    });
    let refreshed = replacements.iter().map(|(id, _)| *id).collect::<Vec<_>>();
    let changed =
        next != before || !added.is_empty() || !renames.is_empty() || !refreshed.is_empty();
    if changed {
        let mut offer = None;
        if offer_match {
            for source in &added {
                offer = media::sequence_offer(store, source).await;
                if offer.is_some() {
                    break;
                }
            }
        }
        let staged = added
            .iter()
            .chain(replacements.iter().map(|(_, source)| source))
            .cloned()
            .collect::<Vec<_>>();
        let committed = store.update(|db| {
            let project = db
                .projects
                .iter_mut()
                .find(|p| p.id == project_id)
                .context("Project not found.")?;
            ensure!(
                project.library_revision == revision,
                "The media library changed during this operation."
            );
            for (id, old) in renames {
                if let Some(source) = project.sources.iter_mut().find(|item| item.id == id)
                    && source.title == default_title(&old)
                    && let Some(location) = next.entries.get(&id)
                {
                    source.title = default_title(&location.place.path);
                }
            }
            project.sources.extend(added);
            project.offer_sequence_format(offer);
            project.library = next.clone();
            project.library_revision += 1;
            project.replace_sources(replacements);
            Ok(())
        });
        if let Err(error) = committed {
            for source in staged {
                media::discard_import(store, &source).await;
            }
            return Err(error);
        }
    }
    let imports = store
        .read(|db| {
            db.projects.iter().find(|p| p.id == project_id).map(|p| {
                imported
                    .iter()
                    .take(120)
                    .filter_map(|id| p.source(*id))
                    .map(|source| p.library.source_json(source))
                    .collect::<Vec<_>>()
            })
        })
        .unwrap_or_default();
    let folder_json = next.folders.iter().map(Folder::public).collect::<Vec<_>>();
    Ok(
        json!({"imported":imports,"refreshed":refreshed,"folders":folder_json,"opened":opened,
        "skipped":skipped,"remaining":remaining,"changed":changed}),
    )
}

#[derive(Clone, Deserialize, Serialize)]
pub struct FolderSelection {
    pub folder: Uuid,
    #[serde(default)]
    pub path: String,
}

#[derive(Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct MediaRemoval {
    pub asset_ids: HashSet<Uuid>,
    pub folders: Vec<FolderSelection>,
}

impl MediaRemoval {
    pub fn is_empty(&self) -> bool {
        self.asset_ids.is_empty() && self.folders.is_empty()
    }
}

pub fn check_draft_changes(
    base: &LibrarySnapshot,
    draft: &LibrarySnapshot,
    live: &LibrarySnapshot,
) -> Result<()> {
    let proposed = draft
        .sources
        .iter()
        .map(|source| (source.id, source))
        .collect::<HashMap<_, _>>();
    let current = live
        .sources
        .iter()
        .map(|source| (source.id, source))
        .collect::<HashMap<_, _>>();
    let message =
        "Media in the staged removal changed in the saved project. Review it and try again.";
    for source in &base.sources {
        match proposed.get(&source.id) {
            None => ensure!(
                current.get(&source.id) == Some(&source)
                    && live.layout.entries.get(&source.id) == base.layout.entries.get(&source.id),
                "{message}"
            ),
            Some(&updated) if updated != source => {
                ensure!(
                    current
                        .get(&source.id)
                        .is_some_and(|&live| live == source || live == updated),
                    "The draft and saved project changed the same media item."
                );
            }
            _ => {}
        }
    }
    let retained = draft
        .layout
        .folders
        .iter()
        .map(|folder| folder.id)
        .collect::<HashSet<_>>();
    let mut scopes = base
        .layout
        .folders
        .iter()
        .filter(|folder| !retained.contains(&folder.id))
        .map(|folder| folder_place(folder, ""))
        .collect::<Vec<_>>();
    scopes.extend(
        draft
            .layout
            .hidden
            .iter()
            .filter(|scope| !base.layout.hidden.contains(scope))
            .cloned(),
    );
    for folder in &base.layout.folders {
        let location = folder_place(folder, "");
        if scopes.iter().any(|scope| contains_place(&location, scope)) {
            ensure!(
                live.layout
                    .folders
                    .iter()
                    .any(|candidate| candidate == folder),
                "{message}"
            );
        }
    }
    ensure!(
        !live
            .layout
            .entries
            .iter()
            .any(|(id, entry)| base.layout.entries.get(id) != Some(entry)
                && scopes
                    .iter()
                    .any(|scope| contains_place(scope, &entry.place))),
        "{message}"
    );
    Ok(())
}

pub struct RemovalPlan {
    sources: HashSet<Uuid>,
    roots: HashSet<Uuid>,
    hidden: Vec<EntryPath>,
    items: Vec<String>,
    clips: usize,
    soundtrack: bool,
}

impl RemovalPlan {
    pub fn public(&self, project: &Project) -> Value {
        let assets = project
            .sources
            .iter()
            .filter(|source| self.sources.contains(&source.id))
            .map(|source| source.id)
            .collect::<Vec<_>>();
        json!({"items":self.items,"asset_ids":assets,"clips":self.clips,"soundtrack":self.soundtrack,
            "revision":project.revision,"library_revision":project.library_revision})
    }

    pub fn apply(self, project: &mut Project) {
        let mut prior = project.snapshot();
        let before = project.capture_library();
        if self.clips > 0 {
            project.materialize_positions();
            project
                .timeline
                .retain(|clip| !self.sources.contains(&clip.source));
            let captioned = project
                .timeline
                .iter()
                .filter(|clip| clip.captioned)
                .map(|clip| clip.id)
                .collect::<HashSet<_>>();
            if captioned.is_empty() {
                project.sequence.captions = None;
            } else if let Some(captions) = &mut project.sequence.captions {
                captions.clip_ids.retain(|id| captioned.contains(id));
                captions.fixes.retain(|id, _| !self.sources.contains(id));
                captions.edits.retain(|edit| {
                    edit.source.map_or_else(
                        || captioned.contains(&edit.clip),
                        |id| !self.sources.contains(&id),
                    )
                });
            }
        }
        if self.soundtrack {
            project.soundtrack = None;
        }
        let dropped = project
            .library
            .folders
            .iter()
            .filter(|folder| self.roots.contains(&folder.id))
            .map(|folder| folder_place(folder, ""))
            .collect::<Vec<_>>();
        project
            .library
            .folders
            .retain(|folder| !self.roots.contains(&folder.id));
        for path in self.hidden {
            if !project
                .library
                .hidden
                .iter()
                .any(|hidden| contains_place(hidden, &path))
            {
                project.library.hidden.push(path);
            }
        }
        project
            .library
            .hidden
            .retain(|hidden| !dropped.iter().any(|root| contains_place(root, hidden)));
        project
            .library
            .entries
            .retain(|id, _| !self.sources.contains(id));
        project
            .library
            .missing
            .retain(|id, _| !self.sources.contains(id));
        project
            .sources
            .retain(|source| !self.sources.contains(&source.id));
        prior.library = Some(LibraryChange {
            before,
            after: project.capture_library(),
        });
        project.record(prior);
        project.library_revision += 1;
    }
}

fn contains_place(root: &EntryPath, entry: &EntryPath) -> bool {
    root.origin == entry.origin && under(&root.path, &entry.path)
}

fn folder_place(folder: &Folder, path: &str) -> EntryPath {
    if let Some(root) = &folder.inbox {
        EntryPath {
            origin: Origin::Inbox,
            path: if path.is_empty() {
                root.clone()
            } else {
                join(root, path)
            },
        }
    } else {
        EntryPath {
            origin: Origin::Upload(folder.id),
            path: path.to_owned(),
        }
    }
}

pub fn plan_removal(project: &Project, request: MediaRemoval) -> Result<RemovalPlan> {
    ensure!(!request.is_empty(), "Select media or folders to remove.");
    ensure!(
        request
            .asset_ids
            .iter()
            .all(|id| project.source(*id).is_some()),
        "Some selected media is no longer in this project."
    );
    let mut folders = request
        .folders
        .into_iter()
        .map(|selection| {
            let root = project
                .library
                .folders
                .iter()
                .find(|folder| folder.id == selection.folder)
                .context("A selected folder is no longer in this project.")?;
            let path = checked_relative(&selection.path, true)?;
            Ok((root, path))
        })
        .collect::<Result<Vec<_>>>()?;
    folders.sort_by_key(|(_, path)| path.len());
    let mut scopes: Vec<(&Folder, String)> = vec![];
    for (folder, path) in folders {
        if !scopes
            .iter()
            .any(|(parent, prefix)| parent.id == folder.id && under(prefix, &path))
        {
            scopes.push((folder, path));
        }
    }
    let scoped = scopes
        .iter()
        .map(|(root, path)| folder_place(root, path))
        .collect::<Vec<_>>();
    let mut sources = request.asset_ids;
    let mut hidden = vec![];
    let mut items = vec![];
    for source in &project.sources {
        let entry = project.library.entries.get(&source.id);
        if entry.is_some_and(|entry| {
            scoped
                .iter()
                .any(|scope| contains_place(scope, &entry.place))
        }) {
            sources.insert(source.id);
        } else if sources.contains(&source.id) {
            items.push(source.title.clone());
            if project.library.locate(source.id).is_some()
                && let Some(entry) = entry
            {
                hidden.push(entry.place.clone());
            }
        }
    }
    let mut roots = HashSet::new();
    for (root, path) in scopes {
        if path.is_empty() {
            roots.insert(root.id);
            items.push(root.name.clone());
        } else {
            hidden.push(folder_place(root, &path));
            items.push(name_of(&path).to_owned());
        }
    }
    let affected = sources
        .iter()
        .filter_map(|id| project.library.locate(*id))
        .map(|(root, _)| root.id)
        .collect::<HashSet<_>>();
    let surviving = project
        .sources
        .iter()
        .filter(|source| !sources.contains(&source.id))
        .filter_map(|source| project.library.locate(source.id))
        .map(|(root, _)| root.id)
        .collect::<HashSet<_>>();
    roots.extend(affected.difference(&surviving));
    let clips = project
        .timeline
        .iter()
        .filter(|clip| sources.contains(&clip.source))
        .map(|clip| clip.link.unwrap_or(clip.id))
        .collect::<HashSet<_>>()
        .len();
    let soundtrack = project.soundtrack.is_some_and(|id| sources.contains(&id));
    Ok(RemovalPlan {
        sources,
        roots,
        hidden,
        items,
        clips,
        soundtrack,
    })
}

pub fn remove(project: &mut Project, folder: Uuid, path: &str) -> Result<()> {
    plan_removal(
        project,
        MediaRemoval {
            folders: vec![FolderSelection {
                folder,
                path: path.to_owned(),
            }],
            ..MediaRemoval::default()
        },
    )?
    .apply(project);
    project.revision += 1;
    Ok(())
}
