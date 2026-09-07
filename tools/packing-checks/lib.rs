//! Compile the production parser and placement core directly, without V8.
#![allow(dead_code)]

#[path = "../../target/celld/crates/celld/env_vars.rs"]
mod env_vars;
#[path = "../../target/celld/crates/logic/isolate.rs"]
mod isolate;
#[path = "../../target/celld/crates/celld/s3_etag.rs"]
mod s3_etag;

#[cfg(test)]
mod packing_tests {
    use super::isolate::{self, IsolateLoad, Placement, PoolLimits, PoolLoad};

    fn pool(density: usize) -> PoolLoad {
        PoolLoad {
            isolates: vec![],
            limits: PoolLimits {
                grow_at: 2,
                shrink_under: 1,
                max_stateless: 4,
                max_requests: None,
                max_cells: density,
            },
        }
    }

    #[test]
    fn lower_density_grows_at_the_configured_boundary() {
        for density in [1, 2, 4, 32] {
            let mut load = pool(density);
            for cells in 1..=40 {
                match isolate::place_cell(&load) {
                    Placement::Grow => load.isolates.push(IsolateLoad {
                        cells: 1,
                        ..Default::default()
                    }),
                    Placement::Existing(id) => load.isolates[id].cells += 1,
                }
                assert_eq!(load.isolates.len(), (cells + density - 1) / density);
                assert!(load.isolates.iter().all(|i| i.cells <= density));
            }
        }
    }

    #[test]
    fn packing_still_fills_live_heaps_and_leaves_retiring_heaps_alone() {
        let mut load = pool(2);
        load.isolates = vec![
            IsolateLoad {
                retiring: true,
                ..Default::default()
            },
            IsolateLoad {
                cells: 1,
                ..Default::default()
            },
            IsolateLoad::default(),
        ];
        assert_eq!(isolate::place_cell(&load), Placement::Existing(1));
        load.isolates[1].cells = 2;
        assert_eq!(isolate::place_cell(&load), Placement::Existing(2));
        load.isolates[2].retiring = true;
        assert_eq!(isolate::place_cell(&load), Placement::Grow);
    }

    #[test]
    fn retirement_requires_empty_heaps_and_freeing_requires_drained_requests() {
        let mut load = pool(1);
        load.isolates = vec![IsolateLoad {
            cells: 1,
            ..Default::default()
        }];
        assert_eq!(isolate::retire(&load), None);
        load.isolates[0].cells = 0;
        assert_eq!(isolate::retire(&load), Some(0));
        load.isolates[0].retiring = true;
        load.isolates[0].requests = 1;
        assert!(!isolate::may_free(&load.isolates[0]));
        load.isolates[0].requests = 0;
        assert!(isolate::may_free(&load.isolates[0]));
    }
}
