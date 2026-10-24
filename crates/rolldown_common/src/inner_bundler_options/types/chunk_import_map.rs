use std::{future::Future, pin::Pin, sync::Arc};

use arcstr::ArcStr;
use derive_more::Debug;
use rolldown_utils::dashmap::FxDashMap;
#[cfg(feature = "deserialize_bundler_options")]
use schemars::JsonSchema;
#[cfg(feature = "deserialize_bundler_options")]
use serde::Deserialize;

use crate::RollupPreRenderedChunk;

pub type ChunkImportMapIncludeFn = dyn Fn(
    &RollupPreRenderedChunk,
  ) -> Pin<Box<dyn Future<Output = anyhow::Result<bool>> + Send + 'static>>
  + Send
  + Sync;

#[derive(Debug, Default, Clone)]
#[cfg_attr(
  feature = "deserialize_bundler_options",
  derive(Deserialize, JsonSchema),
  serde(rename_all = "camelCase", deny_unknown_fields)
)]
pub struct ChunkImportMap {
  pub base_url: Option<String>,
  pub file_name: Option<String>,
  /// Which hashed chunks get a stable name in the import map. Every hashed chunk is mapped
  /// when unset.
  ///
  /// A chunk that is mapped is referenced by its stable name from every other chunk, so changes to
  /// its content do not cascade into its importers' hashes. Its own hash dependencies still do:
  /// an importer's hash covers the hashed chunks reached through a mapped chunk, as those are
  /// the names tools such as Vite's `__vite__mapDeps` embed into the importer after hashing.
  #[cfg_attr(feature = "deserialize_bundler_options", serde(skip), schemars(skip))]
  #[debug(skip)]
  pub include: Option<Arc<ChunkImportMapIncludeFn>>,
  /// Preliminary file name → stable file name, filled by `finalize_assets` and read by the
  /// `builtin:chunk-import-map` plugin when it emits the import map.
  #[cfg_attr(feature = "deserialize_bundler_options", serde(skip), schemars(skip))]
  #[debug(skip)]
  pub stable_names: Arc<FxDashMap<ArcStr, String>>,
}
