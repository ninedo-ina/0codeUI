//! 会话级类名映射表。
//!
//! 同一会话内的同一原始类名必须稳定映射到同一个随机类名，否则同一页面
//! 的多次渲染会出现样式错乱；不同会话 / 不同请求则使用不同映射，避免
//! 通过类名建立跨用户静态指纹。

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use super::rng::Rng;

/// 一个会话的映射状态。
struct SessionEntry {
    class_map: HashMap<String, String>,
    last_used: Instant,
}

/// 会话映射表（进程内、带 TTL 与容量上限）。
pub(crate) struct SessionStore {
    entries: Mutex<HashMap<String, SessionEntry>>,
    ttl: Duration,
    max_entries: usize,
}

impl SessionStore {
    /// 创建映射表。
    pub(crate) fn new(ttl_secs: u64, max_entries: usize) -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
            ttl: Duration::from_secs(ttl_secs.max(1)),
            max_entries: max_entries.max(1),
        }
    }

    /// 获取或创建会话映射，并补齐本次出现的类名。
    pub(crate) fn mapping_for(
        &self,
        session_id: &str,
        originals: &HashSet<String>,
        rng: &mut Rng,
    ) -> HashMap<String, String> {
        let now = Instant::now();
        let mut entries = self
            .entries
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        entries.retain(|_, entry| now.duration_since(entry.last_used) < self.ttl);

        if !entries.contains_key(session_id) && entries.len() >= self.max_entries {
            if let Some(oldest) = entries
                .iter()
                .min_by_key(|(_, entry)| entry.last_used)
                .map(|(key, _)| key.clone())
            {
                entries.remove(&oldest);
            }
        }

        let entry = entries
            .entry(session_id.to_string())
            .or_insert_with(|| SessionEntry {
                class_map: HashMap::new(),
                last_used: now,
            });
        entry.last_used = now;

        fill_mapping(&mut entry.class_map, originals, rng);
        entry.class_map.clone()
    }

    /// 测试 / 运维观测：当前未过期会话数量。
    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        let now = Instant::now();
        let entries = self
            .entries
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        entries
            .values()
            .filter(|entry| now.duration_since(entry.last_used) < self.ttl)
            .count()
    }
}

/// 为无会话请求生成一次性映射（不进入会话表）。
pub(crate) fn ephemeral_mapping(
    originals: &HashSet<String>,
    rng: &mut Rng,
) -> HashMap<String, String> {
    let mut map = HashMap::new();
    fill_mapping(&mut map, originals, rng);
    map
}

fn fill_mapping(map: &mut HashMap<String, String>, originals: &HashSet<String>, rng: &mut Rng) {
    let used: HashSet<String> = map.values().cloned().collect();
    let mut occupied: HashSet<String> = originals.iter().cloned().collect();
    occupied.extend(used);

    for original in originals {
        if map.contains_key(original) {
            continue;
        }
        let mut candidate = rng.class_name();
        while occupied.contains(&candidate) {
            candidate = rng.class_name();
        }
        occupied.insert(candidate.clone());
        map.insert(original.clone(), candidate);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_session_keeps_mapping_stable() {
        let store = SessionStore::new(60, 10);
        let originals: HashSet<String> = ["card", "title"].iter().map(|s| s.to_string()).collect();
        let mut rng = Rng::new();
        let first = store.mapping_for("s1", &originals, &mut rng);
        assert_eq!(store.len(), 1);
        let second = store.mapping_for("s1", &originals, &mut rng);
        assert_eq!(first, second);
        assert_eq!(first.len(), 2);
    }

    #[test]
    fn different_sessions_get_different_mappings() {
        let store = SessionStore::new(60, 10);
        let originals: HashSet<String> = ["card"].iter().map(|s| s.to_string()).collect();
        let mut rng = Rng::new();
        let first = store.mapping_for("s1", &originals, &mut rng);
        let second = store.mapping_for("s2", &originals, &mut rng);
        assert_ne!(first.get("card"), second.get("card"));
    }

    #[test]
    fn new_classes_are_added_without_changing_existing() {
        let store = SessionStore::new(60, 10);
        let mut rng = Rng::new();
        let first: HashSet<String> = ["card"].iter().map(|s| s.to_string()).collect();
        let mapping = store.mapping_for("s1", &first, &mut rng);
        let card = mapping.get("card").cloned().unwrap();

        let mut second = first.clone();
        second.insert("footer".to_string());
        let mapping = store.mapping_for("s1", &second, &mut rng);
        assert_eq!(mapping.get("card"), Some(&card));
        assert!(mapping.contains_key("footer"));
    }
}
