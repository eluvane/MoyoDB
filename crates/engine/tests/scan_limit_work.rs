use moyodb_engine::btree::KvPair;
use moyodb_engine::catalog::ChangeFeedPolicy;
use moyodb_engine::layout::{page_offset, PageKind, PAGE_SIZE};
use moyodb_engine::page::{decode_page, encode_leaf_page};
use moyodb_engine::storage::backend::{FileBackend, FileSet};
use moyodb_engine::value::StoredValue;
use moyodb_engine::{
    Engine, EngineError, MemoryBackend, MemoryBundle, OpenConfig, Result, ScanRange, TxMode,
};
use std::sync::{Arc, Mutex};

const DB_NAME: &str = "scan-limit-work";
const VALUE_LEN: usize = 512;
type ReadTrace = Arc<Mutex<Vec<(u64, usize)>>>;

struct CountingBackend {
    inner: MemoryBackend,
    reads: ReadTrace,
}

impl CountingBackend {
    fn from_bytes(bytes: Vec<u8>) -> Self {
        Self {
            inner: MemoryBackend::from_durable(bytes),
            reads: Arc::new(Mutex::new(Vec::new())),
        }
    }
}

impl FileBackend for CountingBackend {
    fn read_at(&self, offset: u64, len: usize) -> Result<Vec<u8>> {
        self.reads
            .lock()
            .map_err(|_| EngineError::Storage("test read trace poisoned".into()))?
            .push((offset, len));
        self.inner.read_at(offset, len)
    }

    fn write_at(&mut self, offset: u64, bytes: &[u8]) -> Result<()> {
        self.inner.write_at(offset, bytes)
    }

    fn flush(&mut self) -> Result<()> {
        self.inner.flush()
    }

    fn len(&self) -> Result<u64> {
        self.inner.len()
    }

    fn truncate(&mut self, size: u64) -> Result<()> {
        self.inner.truncate(size)
    }

    fn close(&mut self) -> Result<()> {
        self.inner.close()
    }

    fn durable_snapshot(&self) -> Option<Vec<u8>> {
        self.inner.durable_snapshot()
    }
}

struct LeafInfo {
    page_id: u64,
    keys: Vec<Vec<u8>>,
}

fn read_leaves(main: &MemoryBackend, root: u64) -> Result<Vec<LeafInfo>> {
    let mut pages = vec![root];
    let mut leaves = Vec::new();
    while let Some(page_id) = pages.pop() {
        let page = decode_page(&main.read_at(page_offset(page_id), PAGE_SIZE)?)?;
        match page.header.page_kind {
            PageKind::Leaf => leaves.push(LeafInfo {
                page_id,
                keys: page.leaf_cells.into_iter().map(|cell| cell.key).collect(),
            }),
            PageKind::Internal => {
                pages.extend(
                    page.internal_cells
                        .iter()
                        .rev()
                        .map(|cell| cell.child_page_id),
                );
            }
            PageKind::Overflow => panic!("fixture tree path contains an overflow page"),
        }
    }
    Ok(leaves)
}

struct Fixture {
    manifest: Vec<u8>,
    main: Vec<u8>,
    wal: Vec<u8>,
    store_flags: u64,
    leaves: Vec<LeafInfo>,
}

impl Fixture {
    fn new() -> Result<Self> {
        let bundle = MemoryBundle::new();
        let mut engine = Engine::open(DB_NAME, bundle.files(), OpenConfig::default())?;
        let tx = engine.begin_tx(TxMode::Readwrite)?;
        engine.set_change_feed_policy(
            tx,
            ChangeFeedPolicy {
                enabled: false,
                retain_txids: None,
            },
        )?;
        engine.create_store(tx, "kv")?;
        for key in 0u32..32 {
            engine.put(tx, "kv", &key.to_be_bytes(), &vec![0x47; VALUE_LEN])?;
        }
        engine.commit_tx(tx)?;
        engine.checkpoint()?;
        let meta = engine.catalog().get("kv").expect("fixture store").clone();
        let leaves = read_leaves(&bundle.main, meta.store_root_page_id)?;
        assert!(
            leaves.len() >= 3,
            "fixture needs neighboring leaves in both directions"
        );
        assert!(
            leaves[1].keys.len() >= 3,
            "fixture needs TTL, delete and live rows"
        );
        engine.close()?;
        Ok(Self {
            manifest: bundle
                .manifest
                .durable_snapshot()
                .expect("durable manifest"),
            main: bundle.main.durable_snapshot().expect("durable main"),
            wal: bundle.wal.durable_snapshot().expect("durable wal"),
            store_flags: meta.flags,
            leaves,
        })
    }

    fn open(&self) -> Result<TestDb> {
        let main = CountingBackend::from_bytes(self.main.clone());
        let main_reads = Arc::clone(&main.reads);
        let main_bytes = main.inner.clone();
        let engine = Engine::open(
            DB_NAME,
            FileSet::new(
                CountingBackend::from_bytes(self.manifest.clone()),
                main,
                CountingBackend::from_bytes(self.wal.clone()),
            ),
            OpenConfig::default(),
        )?;
        Ok(TestDb {
            engine,
            main_bytes,
            main_reads,
        })
    }

    fn range_from(&self, key: &[u8], reverse: bool, inclusive: bool) -> ScanRange {
        let mut range = ScanRange {
            reverse,
            limit: Some(1),
            ..ScanRange::default()
        };
        if reverse {
            range.gte = Some(self.leaves[0].keys[0].clone());
            if inclusive {
                range.lte = Some(key.to_vec());
            } else {
                range.lt = Some(key.to_vec());
            }
        } else {
            range.lte = Some(
                self.leaves
                    .last()
                    .expect("last leaf")
                    .keys
                    .last()
                    .expect("last key")
                    .clone(),
            );
            if inclusive {
                range.gte = Some(key.to_vec());
            } else {
                range.gt = Some(key.to_vec());
            }
        }
        range
    }

    fn expire_key(&mut self, key: &[u8]) -> Result<()> {
        let leaf = self
            .leaves
            .iter()
            .find(|leaf| leaf.keys.iter().any(|candidate| candidate == key))
            .expect("fixture expiry key");
        let offset = usize::try_from(page_offset(leaf.page_id)).expect("small fixture offset");
        let mut page = decode_page(&self.main[offset..offset + PAGE_SIZE])?;
        let cell = page
            .leaf_cells
            .iter_mut()
            .find(|cell| cell.key == key)
            .expect("fixture expiry cell");
        let mut stored = StoredValue::decode_for_store(self.store_flags, &cell.value)?;
        // A valid expired committed envelope makes this independent of sleeps
        // and of the time spent seeding or compiling the test.
        stored.expires_at_ms = Some(1);
        cell.value = stored.encode_for_store(self.store_flags)?;
        cell.total_value_len = u32::try_from(cell.value.len()).expect("small fixture value");
        let image = encode_leaf_page(
            page.header.page_id,
            page.header.level,
            page.header.right_sibling_page_id,
            &page.leaf_cells,
        )?;
        self.main[offset..offset + PAGE_SIZE].copy_from_slice(&image);
        Ok(())
    }
}

struct TestDb {
    engine: Engine<CountingBackend>,
    main_bytes: MemoryBackend,
    main_reads: ReadTrace,
}

impl TestDb {
    fn take_reads(&self) -> Vec<(u64, usize)> {
        std::mem::take(&mut *self.main_reads.lock().expect("test read trace"))
    }
}

fn assert_limit_work(reverse: bool, staged: bool) -> Result<()> {
    let fixture = Fixture::new()?;
    let leaf = &fixture.leaves[1];
    let key = if reverse {
        leaf.keys.first()
    } else {
        leaf.keys.last()
    }
    .expect("boundary key");
    let range = fixture.range_from(key, reverse, true);
    let mut readonly = fixture.open()?;
    let mut readwrite = fixture.open()?;
    let ro = readonly.engine.begin_tx(TxMode::Readonly)?;
    let rw = readwrite.engine.begin_tx(TxMode::Readwrite)?;
    if staged {
        // put checks the committed key with lookup_prefix. has performs that
        // same lookup in the reference engine, so both caches start alike.
        assert!(readonly.engine.has(ro, "kv", key)?);
        readwrite.engine.put(rw, "kv", key, b"fresh")?;
    }
    readonly.take_reads();
    readwrite.take_reads();
    let ro_rows = readonly.engine.scan(ro, "kv", &range)?;
    let ro_reads = readonly.take_reads();
    let rw_rows = readwrite.engine.scan(rw, "kv", &range)?;
    let rw_reads = readwrite.take_reads();
    readonly.engine.rollback_tx(ro)?;
    readwrite.engine.rollback_tx(rw)?;
    assert_eq!(
        ro_rows,
        vec![KvPair {
            key: key.clone(),
            value: vec![0x47; VALUE_LEN]
        }]
    );
    assert_eq!(
        rw_rows,
        vec![KvPair {
            key: key.clone(),
            value: if staged {
                b"fresh".to_vec()
            } else {
                vec![0x47; VALUE_LEN]
            }
        }]
    );
    if staged {
        assert!(
            ro_reads.is_empty(),
            "reference boundary leaf must already be cached"
        );
    } else {
        assert!(
            ro_reads.contains(&(page_offset(leaf.page_id), PAGE_SIZE)),
            "reference scan must read the cold boundary leaf"
        );
    }
    println!("work scan_limit reverse={reverse} staged={staged}: readonly_page_reads={} readwrite_page_reads={} readonly_offsets={ro_reads:?} readwrite_offsets={rw_reads:?}", ro_reads.len(), rw_reads.len());
    assert_eq!(
        rw_reads, ro_reads,
        "readwrite limit=1 must stop before fetching the neighboring leaf"
    );
    Ok(())
}

#[test]
fn forward_readwrite_limit_reads_only_the_requested_leaf() -> Result<()> {
    assert_limit_work(false, false)
}

#[test]
fn reverse_readwrite_limit_reads_only_the_requested_leaf() -> Result<()> {
    assert_limit_work(true, false)
}

#[test]
fn forward_staged_override_limit_reads_only_the_requested_leaf() -> Result<()> {
    assert_limit_work(false, true)
}

#[test]
fn reverse_staged_override_limit_reads_only_the_requested_leaf() -> Result<()> {
    assert_limit_work(true, true)
}

#[test]
fn limit_counts_live_rows_after_ttl_and_staged_deletes_in_both_directions() -> Result<()> {
    for reverse in [false, true] {
        for staged_expiry in [false, true] {
            let mut fixture = Fixture::new()?;
            let keys = &fixture.leaves[1].keys;
            let (expired, deleted, live) = if reverse {
                (keys[2].clone(), keys[1].clone(), keys[0].clone())
            } else {
                let end = keys.len();
                (
                    keys[end - 3].clone(),
                    keys[end - 2].clone(),
                    keys[end - 1].clone(),
                )
            };
            fixture.expire_key(&expired)?;
            let mut database = fixture.open()?;
            let rw = database.engine.begin_tx(TxMode::Readwrite)?;
            assert!(database.engine.delete(rw, "kv", &deleted)?);
            database.engine.put(rw, "kv", &live, b"fresh")?;
            if staged_expiry {
                database
                    .engine
                    .put_with_ttl(rw, "kv", &expired, b"temporary", Some(0))?;
            }
            let rows =
                database
                    .engine
                    .scan(rw, "kv", &fixture.range_from(&expired, reverse, true))?;
            assert_eq!(
                rows,
                vec![KvPair {
                    key: live.clone(),
                    value: b"fresh".to_vec()
                }]
            );
            database.engine.commit_tx(rw)?;
            database.engine.checkpoint()?;
            let root = database
                .engine
                .catalog()
                .get("kv")
                .expect("committed store")
                .store_root_page_id;
            let keys: Vec<_> = read_leaves(&database.main_bytes, root)?
                .into_iter()
                .flat_map(|leaf| leaf.keys)
                .collect();
            assert!(
                !keys.contains(&expired),
                "observed expired base key must be physically cleaned up"
            );
            assert!(!keys.contains(&deleted), "staged delete must be committed");
            let ro = database.engine.begin_tx(TxMode::Readonly)?;
            assert_eq!(
                database.engine.get(ro, "kv", &live)?,
                Some(b"fresh".to_vec())
            );
            let live_number = u32::from_be_bytes(live.as_slice().try_into().expect("fixture key"));
            let next_number = if reverse {
                live_number - 1
            } else {
                live_number + 1
            };
            let rows =
                database
                    .engine
                    .scan(ro, "kv", &fixture.range_from(&live, reverse, false))?;
            assert_eq!(
                rows,
                vec![KvPair {
                    key: next_number.to_be_bytes().to_vec(),
                    value: vec![0x47; VALUE_LEN]
                }]
            );
            database.engine.rollback_tx(ro)?;
        }
    }
    Ok(())
}
