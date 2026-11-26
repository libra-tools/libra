//! Bounded page-body cache (C-05).
//!
//! Keys include scope and manifest id. Billed bytes count the page body plus
//! fixed per-entry overhead. When the budget is full, least-recently-used
//! pages are evicted; a page that does not fit is skipped, not rejected.

use std::{
    collections::{HashMap, VecDeque},
    sync::{Mutex, OnceLock},
};

/// Process page-cache budget (C-05): 128 MiB including entry overhead.
pub const PAGE_CACHE_BUDGET: usize = 128 * 1024 * 1024;

/// Fixed overhead charged per cached page (key, map node, index bookkeeping).
const PAGE_OVERHEAD: usize = 256;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct PageKey {
    pub scope: String,
    pub manifest_id: String,
    pub page_no: u32,
}

struct Entry {
    bytes: Vec<u8>,
    billed: usize,
}

struct Inner {
    map: HashMap<PageKey, Entry>,
    order: VecDeque<PageKey>,
    billed: usize,
    capacity: usize,
}

pub struct PageCache {
    inner: Mutex<Inner>,
}

impl PageCache {
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            inner: Mutex::new(Inner {
                map: HashMap::new(),
                order: VecDeque::new(),
                billed: 0,
                capacity,
            }),
        }
    }

    pub fn shared() -> &'static Self {
        static CACHE: OnceLock<PageCache> = OnceLock::new();
        CACHE.get_or_init(|| PageCache::with_capacity(PAGE_CACHE_BUDGET))
    }

    #[cfg(test)]
    pub fn billed_bytes(&self) -> usize {
        self.lock().billed
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.lock().map.len()
    }

    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.lock().map.is_empty()
    }

    pub fn get(&self, key: &PageKey) -> Option<Vec<u8>> {
        let mut guard = self.lock();
        if !guard.map.contains_key(key) {
            return None;
        }
        if let Some(pos) = guard.order.iter().position(|candidate| candidate == key)
            && let Some(found) = guard.order.remove(pos)
        {
            guard.order.push_back(found);
        }
        guard.map.get(key).map(|entry| entry.bytes.clone())
    }

    /// Insert or replace. Evicts least-recently-used pages until `bytes` fits.
    /// A single page larger than the budget is not retained and does not fail
    /// the caller.
    pub fn insert(&self, key: PageKey, bytes: Vec<u8>) {
        let cost = page_cost(&key, bytes.len());
        let mut guard = self.lock();
        if let Some(old) = guard.map.remove(&key) {
            guard.billed = guard.billed.saturating_sub(old.billed);
            if let Some(pos) = guard.order.iter().position(|candidate| candidate == &key) {
                guard.order.remove(pos);
            }
        }
        if cost > guard.capacity {
            return;
        }
        while guard.billed.saturating_add(cost) > guard.capacity {
            let Some(victim) = guard.order.pop_front() else {
                break;
            };
            if let Some(old) = guard.map.remove(&victim) {
                guard.billed = guard.billed.saturating_sub(old.billed);
            }
        }
        if guard.billed.saturating_add(cost) > guard.capacity {
            return;
        }
        guard.billed = guard.billed.saturating_add(cost);
        guard.order.push_back(key.clone());
        guard.map.insert(
            key,
            Entry {
                bytes,
                billed: cost,
            },
        );
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }
}

fn page_cost(key: &PageKey, body: usize) -> usize {
    PAGE_OVERHEAD
        .saturating_add(key.scope.len())
        .saturating_add(key.manifest_id.len())
        .saturating_add(std::mem::size_of::<u32>())
        .saturating_add(body)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(manifest_id: &str, page_no: u32) -> PageKey {
        PageKey {
            scope: "local".to_string(),
            manifest_id: manifest_id.to_string(),
            page_no,
        }
    }

    #[test]
    fn oversized_page_is_skipped_and_small_pages_evict() {
        let cache = PageCache::with_capacity(400);
        cache.insert(key("huge", 0), vec![0; 2048]);
        assert_eq!(cache.len(), 0);
        assert_eq!(cache.billed_bytes(), 0);
        cache.insert(key("a", 0), vec![1; 64]);
        assert_eq!(cache.len(), 1);
        cache.insert(key("b", 0), vec![1; 64]);
        assert_eq!(cache.len(), 1);
        assert!(cache.billed_bytes() <= 400);
        assert!(cache.get(&key("a", 0)).is_none());
        assert!(cache.get(&key("b", 0)).is_some());
    }

    fn rss_anon_kb() -> u64 {
        let text = std::fs::read_to_string("/proc/self/status").expect("status");
        for line in text.lines() {
            let Some(rest) = line.strip_prefix("RssAnon:") else {
                continue;
            };
            return rest
                .split_whitespace()
                .next()
                .unwrap()
                .parse()
                .expect("RssAnon");
        }
        panic!("RssAnon missing");
    }

    struct WalkStats {
        disk: u64,
        parse: u64,
        storage: u64,
    }

    #[test]
    fn page_counts_16_256_4096_dual_session_stay_within_budget() {
        assert_eq!(PAGE_CACHE_BUDGET, 128 * 1024 * 1024);
        const SESSION_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        const SESSION_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
        const PAGE_BODY: usize = 64 * 1024;
        let body = vec![b'p'; PAGE_BODY];
        let cache = PageCache::with_capacity(PAGE_CACHE_BUDGET);
        let dir = tempfile::tempdir().unwrap();
        let mut stats = WalkStats {
            disk: 0,
            parse: 0,
            storage: 0,
        };
        let before = rss_anon_kb();
        let mut seen = 0usize;
        for pages in [16usize, 256, 4096] {
            for page_no in seen..pages {
                for manifest_id in [SESSION_A, SESSION_B] {
                    let path = dir.path().join(format!("{manifest_id}-{page_no:08}.json"));
                    std::fs::write(&path, format!(r#"{{"page_no":{page_no}}}"#)).unwrap();
                    let envelope = std::fs::read(&path).unwrap();
                    stats.disk += 1;
                    let parsed: serde_json::Value = serde_json::from_slice(&envelope).unwrap();
                    assert_eq!(parsed["page_no"].as_u64(), Some(page_no as u64));
                    stats.parse += 1;
                    stats.storage += 1;
                    cache.insert(key(manifest_id, page_no as u32), body.clone());
                }
            }
            seen = pages;
            let visits = (pages as u64) * 2;
            assert_eq!(stats.disk, visits, "disk reads at {pages} pages");
            assert_eq!(stats.parse, visits, "parses at {pages} pages");
            assert_eq!(stats.storage, visits, "storage probes at {pages} pages");
            assert!(cache.billed_bytes() <= PAGE_CACHE_BUDGET);
            let delta_kb = rss_anon_kb().saturating_sub(before);
            assert!(
                delta_kb < ((PAGE_CACHE_BUDGET / 1024) + 96 * 1024) as u64,
                "RssAnon grew by {delta_kb} KiB at {pages} pages (billed {})",
                cache.billed_bytes()
            );
            if pages == 16 {
                assert_eq!(cache.len(), 32, "16 pages x 2 sessions fit");
            } else if pages == 256 {
                const {
                    assert!(256 * 2 * PAGE_BODY < PAGE_CACHE_BUDGET);
                }
                assert_eq!(cache.len(), 512, "256 pages x 2 sessions fit");
            } else {
                const {
                    assert!(4096 * 2 * PAGE_BODY > PAGE_CACHE_BUDGET);
                }
                assert!(cache.len() < 4096, "evicted before retaining every page");
                assert!(cache.get(&key(SESSION_A, 0)).is_none());
                assert!(cache.get(&key(SESSION_B, 4095)).is_some());
            }
        }
    }
}
