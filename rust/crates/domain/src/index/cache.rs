use super::*;

// Bump when the cached representation changes shape.
const INDEX_CACHE_FORMAT: u32 = 4;

#[derive(Serialize, Deserialize)]
pub(super) struct CachedFile {
    // Hash of the file's source: the entry is reused only while this still matches.
    pub(super) hash: u64,
    pub(super) entry: IndexEntry,
    pub(super) candidates: Vec<String>,
}

#[derive(Deserialize)]
struct CacheFileIn {
    pub(super) format: u32,
    pub(super) version: String,
    pub(super) fingerprint: u64,
    pub(super) entries: HashMap<String, CachedFile>,
}

#[derive(Serialize)]
struct CacheFileOut<'a> {
    pub(super) format: u32,
    pub(super) version: &'a str,
    pub(super) fingerprint: u64,
    pub(super) entries: HashMap<&'a str, &'a CachedFile>,
}

// FNV-1a: `DefaultHasher`'s algorithm can change between Rust versions, and cache file
// names and fingerprints should stay reproducible.
struct Fnv(u64);

impl std::hash::Hasher for Fnv {
    fn finish(&self) -> u64 {
        self.0
    }

    fn write(&mut self, bytes: &[u8]) {
        for byte in bytes {
            self.0 = (self.0 ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
}

fn hash_of(value: impl std::hash::Hash) -> u64 {
    use std::hash::Hasher;
    let mut hasher = Fnv(0xcbf2_9ce4_8422_2325);
    value.hash(&mut hasher);
    hasher.finish()
}

/// What an entry in the cache is worth to this run: still valid as it was loaded, or
/// freshly computed.
pub(super) enum Slot {
    Kept,
    Fresh(Box<CachedFile>),
}

/// Indexes files, reusing the stored result for any file whose source is unchanged. A
/// missing, unreadable or mismatched cache is ignored, so the index is the same with or
/// without one.
pub(crate) struct FileIndexer<'a> {
    pub(super) fs: &'a dyn crate::ports::FileSystem,
    pub(super) project_root: &'a Path,
    pub(super) config: &'a LinterConfig,
    // The cache and the name this project's entries are kept under in it.
    pub(super) cache: Option<(&'a dyn crate::ports::IndexCache, String)>,
    pub(super) loaded: HashMap<String, CachedFile>,
    pub(super) fingerprint: u64,
    pub(super) slots: Mutex<HashMap<String, Slot>>,
}

impl<'a> FileIndexer<'a> {
    pub(crate) fn new(
        fs: &'a dyn crate::ports::FileSystem,
        project_root: &'a Path,
        config: &'a LinterConfig,
        cache: Option<&'a dyn crate::ports::IndexCache>,
    ) -> Self {
        // Configuration comes only from pyproject.toml, so its text stands for it.
        let pyproject = fs
            .read_to_string(&project_root.join("pyproject.toml"))
            .unwrap_or_default();
        let fingerprint = hash_of((
            INDEX_CACHE_FORMAT,
            env!("CARGO_PKG_VERSION"),
            cache.map_or("", |cache| cache.build_id()),
            project_root.to_string_lossy().as_ref(),
            pyproject,
        ));
        let cache = cache.map(|cache| {
            let name = format!(
                "index-{:016x}.bin",
                hash_of(project_root.to_string_lossy().as_ref())
            );
            (cache, name)
        });
        let loaded = cache
            .as_ref()
            .and_then(|(cache, name)| cache.load(name))
            .and_then(|bytes| rmp_serde::from_slice::<CacheFileIn>(&bytes).ok())
            .filter(|file| {
                file.format == INDEX_CACHE_FORMAT
                    && file.version == env!("CARGO_PKG_VERSION")
                    && file.fingerprint == fingerprint
            })
            .map(|file| file.entries)
            .unwrap_or_default();
        FileIndexer {
            fs,
            project_root,
            config,
            cache,
            loaded,
            fingerprint,
            slots: Mutex::new(HashMap::new()),
        }
    }

    pub(crate) fn index(&self, path: &Path) -> Option<(IndexEntry, Vec<String>)> {
        let source = self.fs.read_to_string(path).ok()?;
        let key = path.to_str()?;
        if self.cache.is_none() {
            return index_source(self.fs, path, &source, self.project_root, self.config)
                .map(|(entry, candidates, _)| (entry, candidates));
        }
        let hash = hash_of(&source);
        if let Some(cached) = self.loaded.get(key).filter(|c| c.hash == hash) {
            if let Ok(mut slots) = self.slots.lock() {
                slots.insert(key.to_string(), Slot::Kept);
            }
            return Some((cached.entry.clone(), cached.candidates.clone()));
        }
        let (entry, candidates, cacheable) =
            index_source(self.fs, path, &source, self.project_root, self.config)?;
        if cacheable {
            let fresh = CachedFile {
                hash,
                entry: entry.clone(),
                candidates: candidates.clone(),
            };
            if let Ok(mut slots) = self.slots.lock() {
                slots.insert(key.to_string(), Slot::Fresh(Box::new(fresh)));
            }
        }
        Some((entry, candidates))
    }

    /// Every path indexed in parallel; results in input order, `None` for a file that
    /// couldn't be read.
    pub(crate) fn index_all(&self, paths: &[PathBuf]) -> Vec<Option<(IndexEntry, Vec<String>)>> {
        crate::parallel::parallel_map_largest_first(
            paths,
            |path| self.fs.metadata(path).map_or(0, |meta| meta.len),
            |path| self.index(path),
        )
    }

    /// Writes the cache back if it changed; best effort. A whole-project run passes `prune`
    /// to drop entries for files it didn't find, a partial run keeps what it didn't visit.
    pub(crate) fn save(&self, prune: bool) {
        let (Some((cache, name)), Ok(slots)) = (&self.cache, self.slots.lock()) else {
            return;
        };
        let fresh = slots
            .values()
            .filter(|s| matches!(s, Slot::Fresh(_)))
            .count();
        let kept = slots.len() - fresh;
        let dropped = prune && kept != self.loaded.len();
        if fresh == 0 && !dropped {
            return;
        }
        let mut entries: HashMap<&str, &CachedFile> = HashMap::with_capacity(slots.len());
        if !prune {
            for (key, file) in &self.loaded {
                entries.insert(key, file);
            }
        }
        for (key, slot) in slots.iter() {
            match slot {
                Slot::Fresh(file) => entries.insert(key, file),
                Slot::Kept => self
                    .loaded
                    .get(key)
                    .and_then(|file| entries.insert(key, file)),
            };
        }
        let out = CacheFileOut {
            format: INDEX_CACHE_FORMAT,
            version: env!("CARGO_PKG_VERSION"),
            fingerprint: self.fingerprint,
            entries,
        };
        let Ok(bytes) = rmp_serde::to_vec(&out) else {
            return;
        };
        cache.store(name, &bytes);
    }
}
