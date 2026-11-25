use std::{
    collections::{BTreeMap, HashMap},
    fs::{self, File, OpenOptions},
    io::{self, BufRead, BufReader, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use anyhow::{anyhow, Context as _, Result};
use chrono::Utc;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use uuid::Uuid;

use crate::types::*;

#[derive(Clone)]
pub struct Store(Arc<Mutex<Disk>>);

pub struct Disk {
    root: PathBuf,
    _lock: File,
    agents: HashMap<Uuid, Agent>,
    index: BTreeMap<u64, Entry>,
    by_request: HashMap<Uuid, u64>,
    last_by_agent: HashMap<Uuid, Status>,
    next_sequence: u64,
    write_failed: bool,
}

struct Entry {
    offset: u64,
    bytes: usize,
    summary: RequestSummary,
}

#[derive(Serialize, Deserialize)]
struct Pending {
    record: Record,
    next_context: Option<Context>,
}

impl Store {
    pub fn open(root: PathBuf) -> Result<Self> {
        Ok(Self(Arc::new(Mutex::new(Disk::open(root)?))))
    }

    pub async fn run<T, F>(&self, operation: F) -> Result<T, Failure>
    where
        T: Send + 'static,
        F: FnOnce(&mut Disk) -> Result<T, Failure> + Send + 'static,
    {
        let shared = self.0.clone();
        tokio::task::spawn_blocking(move || {
            let mut disk = shared
                .lock()
                .map_err(|_| Failure::storage("Storage lock failed"))?;
            operation(&mut disk)
        })
        .await
        .map_err(|error| Failure::storage(error.to_string()))?
    }
}

pub fn data_root() -> Result<PathBuf> {
    if let Some(root) = std::env::var_os("ZEPHYR_DATA_DIR") {
        return Ok(root.into());
    }
    let root = std::env::var_os("LOCALAPPDATA")
        .or_else(|| std::env::var_os("XDG_DATA_HOME"))
        .or_else(|| {
            std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share").into())
        })
        .ok_or_else(|| anyhow!("Local data directory is unavailable"))?;
    Ok(PathBuf::from(root).join("Zephyr"))
}

impl Disk {
    fn open(root: PathBuf) -> Result<Self> {
        fs::create_dir_all(root.join("logs/pending"))?;
        fs::create_dir_all(root.join("agents"))?;
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .read(true)
            .open(root.join(".lock"))?;
        fs2::FileExt::try_lock_exclusive(&lock)
            .context("Zephyr is already using this data directory")?;

        let mut disk = Self {
            root,
            _lock: lock,
            agents: HashMap::new(),
            index: BTreeMap::new(),
            by_request: HashMap::new(),
            last_by_agent: HashMap::new(),
            next_sequence: 1,
            write_failed: false,
        };
        if !disk.settings_path().try_exists()? {
            atomic_json(&disk.settings_path(), &Settings::default())?;
        }

        for file in fs::read_dir(disk.root.join("agents"))? {
            let file = file?;
            let path = file.path().join("config.json");
            if !path.try_exists()? {
                continue;
            }
            let agent: Agent = read_json(&path)?;
            if disk.agent_dir(agent.id) != file.path() {
                return Err(anyhow!(
                    "Agent directory does not match its configuration: {}",
                    path.display()
                ));
            }
            disk.agents.insert(agent.id, agent);
        }

        scan_log(&disk.global_log(), |offset, bytes, summary| {
            disk.index.insert(
                summary.meta.sequence,
                Entry {
                    offset,
                    bytes,
                    summary,
                },
            );
            Ok(())
        })?;
        for entry in disk.index.values() {
            let meta = &entry.summary.meta;
            disk.by_request.insert(meta.request_id, meta.sequence);
            if let Some(id) = meta.agent_id {
                disk.last_by_agent.insert(id, meta.status);
            }
        }
        if let Some(sequence) = disk.index.keys().next_back() {
            disk.next_sequence = sequence + 1;
        }

        let mut pending = Vec::<Pending>::new();
        for file in fs::read_dir(disk.root.join("logs/pending"))? {
            let path = file?.path();
            if path.extension().is_some_and(|ext| ext == "json") {
                pending.push(read_json(&path)?);
            }
        }
        pending.sort_unstable_by_key(|pending| pending.record.meta.sequence);
        for mut job in pending {
            let meta = &mut job.record.meta;
            disk.next_sequence = disk.next_sequence.max(meta.sequence + 1);
            if matches!(meta.status, Status::Queued | Status::Generating) {
                meta.status = Status::Interrupted;
                meta.completed_at = Some(Utc::now());
                job.record.error = Some(Failure::new(
                    "interrupted",
                    "Zephyr stopped before the request completed",
                ));
                job.next_context = None;
            }
            disk.commit(job, true)?;
        }
        Ok(disk)
    }

    fn settings_path(&self) -> PathBuf {
        self.root.join("settings.json")
    }
    fn agent_dir(&self, id: Uuid) -> PathBuf {
        self.root.join("agents").join(id.to_string())
    }
    fn global_log(&self) -> PathBuf {
        self.root.join("logs/requests.jsonl")
    }
    fn pending_path(&self, id: Uuid) -> PathBuf {
        self.root.join("logs/pending").join(format!("{id}.json"))
    }

    fn writable(&self) -> Result<(), Failure> {
        if self.write_failed {
            return Err(Failure::storage(
                "A request write failed. Restart Zephyr to replay the pending journal.",
            ));
        }
        Ok(())
    }

    pub fn settings(&mut self) -> Result<Settings, Failure> {
        let settings: Settings = read_json(&self.settings_path()).map_err(storage)?;
        settings.validate()?;
        Ok(settings)
    }

    pub fn network_settings(&mut self) -> Result<crate::connections::NetworkSettings, Failure> {
        let path = self.root.join("connections.json");
        if path.try_exists().map_err(storage)? {
            return read_json(&path).map_err(storage);
        }
        Ok(Default::default())
    }

    pub fn save_network_settings(
        &mut self,
        settings: crate::connections::NetworkSettings,
    ) -> Result<(), Failure> {
        self.writable()?;
        atomic_json(&self.root.join("connections.json"), &settings).map_err(storage)
    }

    pub fn save_settings(&mut self, settings: Settings) -> Result<Settings, Failure> {
        self.writable()?;
        settings.validate()?;
        atomic_json(&self.settings_path(), &settings).map_err(storage)?;
        Ok(settings)
    }

    pub fn agents(&mut self, include_deleted: bool) -> Vec<(Agent, Option<Status>)> {
        let mut agents: Vec<_> = self
            .agents
            .values()
            .filter(|agent| include_deleted || !agent.deleted)
            .map(|agent| (agent.clone(), self.last_by_agent.get(&agent.id).copied()))
            .collect();
        agents.sort_by(|(a, _), (b, _)| {
            a.config
                .name
                .to_lowercase()
                .cmp(&b.config.name.to_lowercase())
                .then(a.created_at.cmp(&b.created_at))
        });
        agents
    }

    pub fn agent(&self, id: Uuid, include_deleted: bool) -> Result<Agent, Failure> {
        self.agents
            .get(&id)
            .filter(|agent| include_deleted || !agent.deleted)
            .cloned()
            .ok_or_else(|| Failure::missing("Agent does not exist"))
    }

    pub fn create(&mut self, config: AgentConfig) -> Result<Agent, Failure> {
        self.writable()?;
        config.validate()?;
        let agent = Agent {
            id: Uuid::new_v4(),
            created_at: Utc::now(),
            deleted: false,
            config,
        };
        let directory = self.agent_dir(agent.id);
        fs::create_dir_all(&directory).map_err(storage)?;
        atomic_json(&directory.join("context.json"), &Context::default()).map_err(storage)?;
        append_file(&directory.join("history.jsonl"), b"").map_err(storage)?;
        self.set_agent(agent)
    }

    pub fn set_agent(&mut self, agent: Agent) -> Result<Agent, Failure> {
        self.writable()?;
        agent.config.validate()?;
        atomic_json(&self.agent_dir(agent.id).join("config.json"), &agent).map_err(storage)?;
        self.agents.insert(agent.id, agent.clone());
        Ok(agent)
    }

    pub fn context(&mut self, id: Uuid) -> Result<Context, Failure> {
        self.agent(id, false)?;
        read_json(&self.agent_dir(id).join("context.json")).map_err(storage)
    }

    pub fn reset(&mut self, id: Uuid) -> Result<Context, Failure> {
        self.writable()?;
        let mut context = self.context(id)?;
        context.messages.clear();
        context.updated_at = Some(Utc::now());
        atomic_json(&self.agent_dir(id).join("context.json"), &context).map_err(storage)?;
        Ok(context)
    }

    pub fn admit(&mut self, mut record: Record) -> Result<Record, Failure> {
        self.writable()?;
        record.meta.sequence = self.next_sequence;
        self.next_sequence += 1;
        self.journal(&record)?;
        Ok(record)
    }

    pub fn journal(&mut self, record: &Record) -> Result<(), Failure> {
        self.writable()?;
        atomic_json(
            &self.pending_path(record.meta.request_id),
            &Pending {
                record: record.clone(),
                next_context: None,
            },
        )
        .map_err(storage)
    }

    pub fn finish(&mut self, record: Record, next_context: Option<Context>) -> Result<(), Failure> {
        self.writable()?;
        if let Err(error) = self.commit(
            Pending {
                record,
                next_context,
            },
            false,
        ) {
            self.write_failed = true;
            return Err(storage(error));
        }
        Ok(())
    }

    fn commit(&mut self, pending: Pending, recovery: bool) -> Result<()> {
        let record = &pending.record;
        let meta = &record.meta;
        let path = self.pending_path(meta.request_id);
        if !recovery {
            atomic_json(&path, &pending)?;
        }

        if !self.by_request.contains_key(&meta.request_id) {
            let mut line = serde_json::to_vec(record)?;
            line.push(b'\n');
            let offset = append_file(&self.global_log(), &line)?;
            let summary = RequestSummary {
                meta: meta.clone(),
                model_version: record.model_version.clone(),
                metrics: record.metrics.clone(),
                error: record.error.clone(),
                done_reason: record.done_reason.clone(),
            };
            self.by_request.insert(meta.request_id, meta.sequence);
            self.index.insert(
                meta.sequence,
                Entry {
                    offset,
                    bytes: line.len(),
                    summary,
                },
            );
            if let Some(id) = meta.agent_id {
                self.last_by_agent.insert(id, meta.status);
            }
        }

        if let Some(id) = meta.agent_id {
            let log = self.agent_dir(id).join("history.jsonl");
            let mut mirrored = false;
            if recovery {
                scan_log(&log, |_, _, summary| {
                    mirrored |= summary.meta.request_id == meta.request_id;
                    Ok(())
                })?;
            }
            if !mirrored {
                let mut line = serde_json::to_vec(record)?;
                line.push(b'\n');
                append_file(&log, &line)?;
            }
            if let Some(context) = pending.next_context {
                let path = self.agent_dir(id).join("context.json");
                let current: Context = read_json(&path)?;
                if current.last_sequence < context.last_sequence {
                    atomic_json(&path, &context)?;
                }
            }
        }
        fs::remove_file(path)?;
        Ok(())
    }

    pub fn page(&mut self, query: LogQuery) -> Page<RequestSummary> {
        let limit = query.limit.unwrap_or(30).clamp(1, 100);
        let mut matches = self
            .index
            .range(..query.before.unwrap_or(u64::MAX))
            .rev()
            .map(|(_, entry)| &entry.summary)
            .filter(|summary| query.matches(&summary.meta));
        let items: Vec<_> = matches.by_ref().take(limit).cloned().collect();
        let next_before = if matches.next().is_some() {
            items.last().map(|summary| summary.meta.sequence)
        } else {
            None
        };
        Page { items, next_before }
    }

    pub fn record(&mut self, id: Uuid) -> Result<Record, Failure> {
        self.read_record(id)
    }

    pub fn turn(&mut self, id: Uuid) -> Result<impl Serialize, Failure> {
        #[derive(Deserialize, Serialize)]
        struct Turn {
            #[serde(flatten)]
            summary: RequestSummary,
            system_prompt: Option<String>,
            client_request: Messages,
            response: Option<Message>,
        }
        #[derive(Deserialize, Serialize)]
        struct Messages {
            messages: Vec<Message>,
        }
        self.read_record::<Turn>(id)
    }

    fn read_record<T: DeserializeOwned>(&self, id: Uuid) -> Result<T, Failure> {
        let entry = self
            .by_request
            .get(&id)
            .and_then(|sequence| self.index.get(sequence))
            .ok_or_else(|| Failure::missing("Request does not exist"))?;
        let mut file = File::open(self.global_log()).map_err(storage)?;
        file.seek(SeekFrom::Start(entry.offset)).map_err(storage)?;
        let mut bytes = vec![0; entry.bytes];
        file.read_exact(&mut bytes).map_err(storage)?;
        serde_json::from_slice(&bytes).map_err(storage)
    }
}

fn storage(error: impl std::fmt::Display) -> Failure {
    Failure::storage(error.to_string())
}

fn read_json<T: DeserializeOwned>(path: &Path) -> Result<T> {
    let read = || -> Result<T> { Ok(serde_json::from_reader(BufReader::new(File::open(path)?))?) };
    read().with_context(|| format!("Cannot read {}", path.display()))
}

fn atomic_json(path: &Path, data: &impl Serialize) -> Result<()> {
    let write = || -> Result<()> {
        let mut temp = tempfile::NamedTempFile::new_in(path.parent().unwrap())?;
        serde_json::to_writer(temp.as_file_mut(), data)?;
        temp.as_file().sync_all()?;
        temp.persist(path)?;
        sync_directory(path.parent().unwrap())?;
        Ok(())
    };
    write().with_context(|| format!("Cannot write {}", path.display()))
}

fn append_file(path: &Path, bytes: &[u8]) -> io::Result<u64> {
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    let offset = file.metadata()?.len();
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(offset)
}

fn scan_log(
    path: &Path,
    mut visit: impl FnMut(u64, usize, RequestSummary) -> Result<()>,
) -> Result<()> {
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path)?;
    let length = file.metadata()?.len();
    let mut reader = BufReader::new(file);
    let mut offset = 0;
    let mut line = Vec::new();
    while offset < length {
        line.clear();
        let bytes = reader.read_until(b'\n', &mut line)?;
        if line.last() != Some(&b'\n') {
            // The durable journal can replay an append interrupted mid-write.
            reader.get_ref().set_len(offset)?;
            reader.get_ref().sync_all()?;
            break;
        }
        let summary = serde_json::from_slice(&line)
            .with_context(|| format!("Corrupted record in {} at byte {offset}", path.display()))?;
        visit(offset, bytes, summary)?;
        offset += bytes as u64;
    }
    Ok(())
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> io::Result<()> {
    File::open(path)?.sync_all()
}

#[cfg(not(unix))]
fn sync_directory(_: &Path) -> io::Result<()> {
    Ok(())
}
