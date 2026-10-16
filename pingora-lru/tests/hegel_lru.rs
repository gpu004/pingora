// Hegel property test: pingora_lru::Lru<T, 1> (single shard) vs a model
// LRU (VecDeque order + BTreeMap). admit promotes existing keys and
// clamps weight to >= 1; evict_shard pops the least-recently-used.

use hegel::generators as gs;
use hegel::TestCase;
use pingora_lru::Lru;
use std::collections::{BTreeMap, VecDeque};

struct Model {
    order: VecDeque<u64>, // front = most recent
    map: BTreeMap<u64, (u8, usize)>,
}

impl Model {
    fn admit(&mut self, k: u64, d: u8, w: usize) {
        let w = w.max(1);
        self.order.retain(|x| *x != k);
        self.order.push_front(k);
        self.map.insert(k, (d, w));
    }
    fn promote(&mut self, k: u64) -> bool {
        if self.map.contains_key(&k) {
            self.order.retain(|x| *x != k);
            self.order.push_front(k);
            true
        } else {
            false
        }
    }
    fn evict(&mut self) -> Option<(u8, usize)> {
        let k = self.order.pop_back()?;
        Some(self.map.remove(&k).unwrap())
    }
    fn remove(&mut self, k: u64) -> Option<(u8, usize)> {
        self.order.retain(|x| *x != k);
        self.map.remove(&k)
    }
    fn weight(&self) -> usize {
        self.map.values().map(|(_, w)| w).sum()
    }
}

#[hegel::test(test_cases = 400)]
fn l1_lru_matches_model(tc: TestCase) {
    let lru = Lru::<u8, 1>::with_capacity(usize::MAX, 1024);
    let mut model = Model {
        order: VecDeque::new(),
        map: BTreeMap::new(),
    };

    let n_ops = tc.draw(gs::integers::<usize>().min_value(1).max_value(40));
    for _ in 0..n_ops {
        let key = tc.draw(gs::integers::<u64>().min_value(0).max_value(15));
        match tc.draw(gs::integers::<u8>().min_value(0).max_value(5)) {
            0 | 1 => {
                // admit
                let data = tc.draw(gs::integers::<u8>());
                let w = tc.draw(gs::integers::<usize>().min_value(0).max_value(10));
                lru.admit(key, data, w);
                model.admit(key, data, w);
            }
            2 => {
                assert_eq!(lru.promote(key), model.promote(key));
            }
            3 => {
                let got = lru.evict_shard(0);
                let exp = model.evict();
                assert_eq!(got.is_some(), exp.is_some());
                if let (Some((d, w)), Some((ed, ew))) = (got, exp) {
                    assert_eq!((d, w), (ed, ew), "evicted entry");
                }
            }
            4 => {
                let got = lru.remove(key);
                let exp = model.remove(key);
                assert_eq!(got.is_some(), exp.is_some());
                if let (Some((d, w)), Some((ed, ew))) = (got, exp) {
                    assert_eq!((d, w), (ed, ew), "removed entry");
                }
            }
            5 => {
                assert_eq!(lru.peek(key), model.map.contains_key(&key));
                assert_eq!(lru.peek_weight(key), model.map.get(&key).map(|(_, w)| *w));
            }
            _ => unreachable!(),
        }
        assert_eq!(lru.len(), model.map.len(), "len after ops");
        assert_eq!(lru.weight(), model.weight(), "weight after ops");
    }
}
