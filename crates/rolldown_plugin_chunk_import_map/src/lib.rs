use std::{borrow::Cow, sync::Arc};

use arcstr::ArcStr;
use rolldown_common::{EmittedAsset, Output};
use rolldown_plugin::{HookUsage, Plugin};
use rolldown_utils::{dashmap::FxDashMap, rustc_hash::FxHashMapExt as _};
use rustc_hash::FxHashMap;

/// Emits the chunk import map. Which chunks are mapped, and how, is decided while hashing
/// (`finalize_assets`), because a mapped chunk changes how its importers are hashed; this
/// plugin only writes the result out once file names are final.
#[derive(Debug, Default)]
pub struct ChunkImportMapPlugin {
  pub base_url: Option<String>,
  pub file_name: Option<String>,
  /// Preliminary file name → stable file name, shared with `ChunkImportMap::stable_names`.
  pub stable_names: Arc<FxDashMap<ArcStr, String>>,
}

impl Plugin for ChunkImportMapPlugin {
  fn name(&self) -> Cow<'static, str> {
    Cow::Borrowed("builtin:chunk-import-map")
  }

  fn register_hook_usage(&self) -> HookUsage {
    HookUsage::GenerateBundle
  }

  async fn generate_bundle(
    &self,
    ctx: &rolldown_plugin::PluginContext,
    args: &mut rolldown_plugin::HookGenerateBundleArgs<'_>,
  ) -> rolldown_plugin::HookNoopReturn {
    if self.stable_names.is_empty() {
      return Ok(());
    }

    let base_url = self.base_url.as_deref().unwrap_or("/");
    let mut chunk_import_map = FxHashMap::with_capacity(self.stable_names.len());
    for output in args.bundle.iter() {
      let Output::Chunk(chunk) = output else { continue };
      if let Some(stable_name) = self.stable_names.get(chunk.preliminary_filename.as_str()) {
        chunk_import_map.insert(
          rolldown_utils::concat_string!(base_url, stable_name.as_str()),
          rolldown_utils::concat_string!(base_url, chunk.filename),
        );
      }
    }

    ctx
      .emit_file_async(EmittedAsset {
        file_name: Some(
          self.file_name.as_ref().map_or(arcstr::literal!("importmap.json"), ArcStr::from),
        ),
        source: (serde_json::to_string(&serde_json::json!({ "imports": chunk_import_map }))?)
          .into(),
        ..Default::default()
      })
      .await?;

    Ok(())
  }

  fn generate_bundle_meta(&self) -> Option<rolldown_plugin::PluginHookMeta> {
    Some(rolldown_plugin::PluginHookMeta { order: Some(rolldown_plugin::PluginOrder::Pre) })
  }
}
