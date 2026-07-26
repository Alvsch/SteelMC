//! Server-global persistent random sequences used by loot tables and other named RNG streams.

use std::{collections::BTreeMap, io, path::Path, str::FromStr};

use rustc_hash::FxHashMap;
use serde::{Deserialize, Serialize};
use steel_utils::{
    Identifier,
    locks::SyncMutex,
    random::{RandomSource, xoroshiro::Xoroshiro},
    saved_data::{SavedDataManager, names as saved_data_names},
};

/// Vanilla's server-owned `RandomSequences` saved data.
pub(crate) struct RandomSequences {
    world_seed: i64,
    saved_data: SavedDataManager,
    inner: SyncMutex<RandomSequencesInner>,
}

struct RandomSequencesInner {
    salt: i32,
    include_world_seed: bool,
    include_sequence_id: bool,
    sequences: FxHashMap<String, RandomSource>,
    revision: u64,
    saved_revision: u64,
}

#[derive(Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct PersistentRandomSequences {
    salt: i32,
    include_world_seed: bool,
    include_sequence_id: bool,
    sequences: BTreeMap<String, PersistentRandomSequence>,
}

impl Default for PersistentRandomSequences {
    fn default() -> Self {
        Self {
            salt: 0,
            include_world_seed: true,
            include_sequence_id: true,
            sequences: BTreeMap::new(),
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PersistentRandomSequence {
    /// The two signed longs encoded by Vanilla's xoroshiro codec.
    source: [i64; 2],
}

impl RandomSequences {
    /// Loads the one random-sequence map shared by every world on the server.
    pub(crate) async fn load(world_seed: i64, save_root: &Path) -> io::Result<Self> {
        let saved_data = SavedDataManager::new(Some(save_root));
        let persistent = saved_data
            .load_or_default(saved_data_names::RANDOM_SEQUENCES)
            .await?;
        Self::from_persistent(world_seed, saved_data, persistent)
    }

    /// Creates an unpersisted sequence map for an independently constructed world.
    pub(crate) fn ephemeral(world_seed: i64) -> Self {
        Self {
            world_seed,
            saved_data: SavedDataManager::new(None),
            inner: SyncMutex::new(RandomSequencesInner {
                salt: 0,
                include_world_seed: true,
                include_sequence_id: true,
                sequences: FxHashMap::default(),
                revision: 0,
                saved_revision: 0,
            }),
        }
    }

    fn from_persistent(
        world_seed: i64,
        saved_data: SavedDataManager,
        persistent: PersistentRandomSequences,
    ) -> io::Result<Self> {
        let mut sequences = FxHashMap::default();
        for (key, sequence) in persistent.sequences {
            let identifier = Identifier::from_str(&key).map_err(|error| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("invalid random-sequence identifier {key:?}: {error}"),
                )
            })?;
            let [seed_lo, seed_hi] = sequence.source;
            sequences.insert(
                identifier.to_string(),
                RandomSource::Xoroshiro(Xoroshiro::from_state(seed_lo as u64, seed_hi as u64)),
            );
        }

        Ok(Self {
            world_seed,
            saved_data,
            inner: SyncMutex::new(RandomSequencesInner {
                salt: persistent.salt,
                include_world_seed: persistent.include_world_seed,
                include_sequence_id: persistent.include_sequence_id,
                sequences,
                revision: 0,
                saved_revision: 0,
            }),
        })
    }

    /// Runs an operation against the persistent stream for `key`.
    pub(crate) fn with_sequence<T>(
        &self,
        key: &Identifier,
        operation: impl FnOnce(&mut RandomSource) -> T,
    ) -> T {
        let mut inner = self.inner.lock();
        let salt = inner.salt;
        let include_world_seed = inner.include_world_seed;
        let include_sequence_id = inner.include_sequence_id;
        let key = key.to_string();
        let random = inner.sequences.entry(key.clone()).or_insert_with(|| {
            Self::create_sequence(
                self.world_seed,
                salt,
                include_world_seed,
                include_sequence_id,
                &key,
            )
        });
        let result = operation(random);
        inner.revision = inner.revision.wrapping_add(1);
        result
    }

    fn create_sequence(
        world_seed: i64,
        salt: i32,
        include_world_seed: bool,
        include_sequence_id: bool,
        key: &str,
    ) -> RandomSource {
        let seed = (if include_world_seed { world_seed } else { 0 }) ^ i64::from(salt);
        let random = if include_sequence_id {
            Xoroshiro::from_seed_with_key(seed as u64, key)
        } else {
            Xoroshiro::from_seed(seed as u64)
        };
        RandomSource::Xoroshiro(random)
    }

    /// Persists changed sequence states. Returns whether a write was needed.
    pub(crate) async fn save(&self) -> io::Result<bool> {
        let Some((revision, persistent)) = self.persistent_snapshot()? else {
            return Ok(false);
        };
        self.saved_data
            .save(saved_data_names::RANDOM_SEQUENCES, &persistent)
            .await?;

        let mut inner = self.inner.lock();
        if inner.revision == revision {
            inner.saved_revision = revision;
        }
        Ok(true)
    }

    fn persistent_snapshot(&self) -> io::Result<Option<(u64, PersistentRandomSequences)>> {
        let inner = self.inner.lock();
        if inner.revision == inner.saved_revision {
            return Ok(None);
        }

        let mut sequences = BTreeMap::new();
        for (key, source) in &inner.sequences {
            let RandomSource::Xoroshiro(random) = source else {
                return Err(io::Error::other(
                    "random-sequence map contained a non-xoroshiro source",
                ));
            };
            let (seed_lo, seed_hi) = random.state();
            sequences.insert(
                key.clone(),
                PersistentRandomSequence {
                    source: [seed_lo as i64, seed_hi as i64],
                },
            );
        }

        Ok(Some((
            inner.revision,
            PersistentRandomSequences {
                salt: inner.salt,
                include_world_seed: inner.include_world_seed,
                include_sequence_id: inner.include_sequence_id,
                sequences,
            },
        )))
    }
}

#[cfg(test)]
mod tests {
    use steel_utils::random::Random;

    use super::*;

    #[test]
    fn restored_sequence_continues_from_its_persisted_state() {
        let key = Identifier::vanilla_static("chests/simple_dungeon");
        let sequences = RandomSequences::ephemeral(12_345);
        sequences.with_sequence(&key, |random| {
            random.next_i64();
            random.next_i32();
        });
        let snapshot = sequences.persistent_snapshot();
        let Ok(Some((_, persistent))) = snapshot else {
            panic!("used sequence should produce a persistent snapshot");
        };
        let expected = sequences.with_sequence(&key, Random::next_i64);

        let restored =
            RandomSequences::from_persistent(12_345, SavedDataManager::new(None), persistent);
        let Ok(restored) = restored else {
            panic!("valid sequence snapshot should restore");
        };
        assert_eq!(restored.with_sequence(&key, Random::next_i64), expected);
    }
}
