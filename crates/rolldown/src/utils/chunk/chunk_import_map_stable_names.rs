//! Stable names for chunks listed in the chunk import map, and their effect on chunk hashing.
//!
//! A mapped chunk is referenced from other chunks by a stable name whose `[hash]` slot is derived
//! from the chunk's identity rather than its content, so the chunk's content can change without
//! renaming any importer. The import map (`builtin:chunk-import-map`) maps the stable name to the
//! content-hashed file name.
//!
//! For hashing, a mapped chunk is *transparent*: an importer's final hash does not include the
//! mapped chunk's content hash, but it does include the content hashes of the hashed chunks the
//! mapped chunk depends on (through any number of mapped chunks). Those names end up in the
//! importer's bytes after hashing — Vite writes every chunk reached through static imports of a
//! dynamic import target into the importer's `__vite__mapDeps` in `generateBundle` — so a hash
//! that stopped at the stable name would leave two builds emitting different bytes under one name.
//! See `internal-docs/chunk-hash/implementation.md`.

use std::{hash::Hash, path::Path};

use oxc_index::IndexVec;
use rolldown_common::{
  ChunkImportMap, HashCharacters, InsChunkIdx, InstantiationKind, ModuleId,
  NormalizedBundlerOptions, RollupPreRenderedChunk, StrOrBytes,
};
use rolldown_error::BuildResult;
use rolldown_utils::{
  hash_placeholder::{HASH_PLACEHOLDER_LEFT_FINDER, find_hash_placeholders},
  indexmap::FxIndexSet,
  rayon::{IntoParallelRefMutIterator, ParallelIterator},
  xxhash::encode_hash_with_base,
};
use rustc_hash::{FxHashMap, FxHashSet};
use xxhash_rust::xxh3::Xxh3;

use crate::{
  chunk_graph::ChunkGraph,
  stages::link_stage::LinkStageOutput,
  type_alias::{IndexChunkToInstances, IndexInstantiatedChunks},
};

/// Hash placeholder → identity-derived hash of the same width, for every mapped chunk.
pub type StableHashByPlaceholder = FxHashMap<String, String>;

/// A module's contribution to a stable name: filesystem ids relative to `cwd`, so the stable
/// name is the same wherever the project is built; other ids (virtual modules) verbatim. The
/// flag keeps the two from colliding. Matches `builtin:chunk-import-map` before this logic
/// moved here.
fn module_key(module_id: &ModuleId, cwd: &Path) -> (bool, String) {
  if module_id.is_path() {
    (true, module_id.stabilize(cwd).as_str().to_string())
  } else {
    (false, module_id.as_str().to_string())
  }
}

/// Decides which hashed chunks are mapped and records each stable name in
/// `ChunkImportMap::stable_names` (preliminary file name → stable file name).
pub async fn collect_stable_hashes(
  chunk_import_map: &ChunkImportMap,
  chunk_graph: &ChunkGraph,
  link_output: &LinkStageOutput,
  index_instantiated_chunks: &IndexInstantiatedChunks,
  hash_characters: HashCharacters,
  options: &NormalizedBundlerOptions,
) -> BuildResult<StableHashByPlaceholder> {
  chunk_import_map.stable_names.clear();
  let base = hash_characters.base();
  let mut stable_hash_by_placeholder = StableHashByPlaceholder::default();
  let mut used_names = FxHashSet::default();

  for ins_chunk in index_instantiated_chunks {
    if ins_chunk.preliminary_filename.hash_placeholder().is_none() {
      continue;
    }
    let InstantiationKind::Ecma(ecma_meta) = &ins_chunk.kind else { continue };
    let rendered = &ecma_meta.rendered_chunk;

    if let Some(include) = &chunk_import_map.include {
      let pre_rendered = RollupPreRenderedChunk {
        name: rendered.name.clone(),
        is_entry: rendered.is_entry,
        is_dynamic_entry: rendered.is_dynamic_entry,
        facade_module_id: rendered.facade_module_id.clone(),
        module_ids: rendered.module_ids.clone(),
        exports: rendered.exports.clone(),
      };
      if !include(&pre_rendered).await? {
        continue;
      }
    }

    let hasher = match &rendered.facade_module_id {
      Some(module_id) => {
        let mut hasher = Xxh3::with_seed(0);
        module_key(module_id, &options.cwd).hash(&mut hasher);
        hasher
      }
      None => {
        let mut hasher = Xxh3::with_seed(1);
        if used_names.contains(&rendered.name) {
          // Several common chunks can share a name. Key later ones by the smallest module among
          // their original members (not `module_ids`, which inlined copies extend).
          let logical_chunk = &chunk_graph.chunk_table[ins_chunk.originate_from];
          let Some(key) = logical_chunk
            .modules
            .iter()
            .map(|&idx| module_key(link_output.module_table[idx].id(), &options.cwd))
            .min()
          else {
            continue;
          };
          key.hash(&mut hasher);
        } else {
          used_names.insert(rendered.name.clone());
          rendered.name.hash(&mut hasher);
        }
        hasher
      }
    };
    let identity_hash = encode_hash_with_base(&hasher.digest128().to_le_bytes(), base);

    let preliminary_filename = ins_chunk.preliminary_filename.as_str();
    let mut stable_filename = preliminary_filename.to_string();
    for (start, end, placeholder) in
      find_hash_placeholders(preliminary_filename, &HASH_PLACEHOLDER_LEFT_FINDER)
    {
      let hash = &identity_hash[..end - start];
      stable_filename.replace_range(start..end, hash);
      stable_hash_by_placeholder.insert(placeholder.to_string(), hash.to_string());
    }
    chunk_import_map.stable_names.insert(preliminary_filename.into(), stable_filename);
  }

  Ok(stable_hash_by_placeholder)
}

/// Writes the stable hash over every mapped placeholder in chunk contents. Same width, so
/// sourcemaps stay valid, and done before the standalone content hash so the stable-name bytes an
/// importer ends up with are part of its hash.
pub fn substitute_stable_hashes(
  index_instantiated_chunks: &mut IndexInstantiatedChunks,
  stable_hash_by_placeholder: &StableHashByPlaceholder,
) {
  index_instantiated_chunks.par_iter_mut().for_each(|ins_chunk| {
    let StrOrBytes::Str(content) = &mut ins_chunk.content else { return };
    let replacements: Vec<(usize, usize, &str)> =
      find_hash_placeholders(content, &HASH_PLACEHOLDER_LEFT_FINDER)
        .filter_map(|(start, end, placeholder)| {
          stable_hash_by_placeholder.get(placeholder).map(|hash| (start, end, hash.as_str()))
        })
        .collect();
    if replacements.is_empty() {
      return;
    }
    // SAFETY: each placeholder is ASCII and is replaced with ASCII of the same length.
    let bytes = unsafe { content.as_bytes_mut() };
    for (start, end, hash) in replacements {
      debug_assert_eq!(hash.len(), end - start);
      bytes[start..end].copy_from_slice(hash.as_bytes());
    }
  });
}

/// The chunks a mapped chunk statically imports. Only these can reach an importer's bytes
/// through the stable name: post-hash tooling walks static imports (Vite's `__vite__mapDeps` lists
/// the static closure of each dynamic import target), never a mapped chunk's own dynamic
/// imports, which are resolved from that chunk's content with its own name.
pub fn static_dependencies_of_mapped(
  mapped: &FxHashSet<InsChunkIdx>,
  index_instantiated_chunks: &IndexInstantiatedChunks,
  chunk_graph: &ChunkGraph,
  index_chunk_to_instances: &IndexChunkToInstances,
) -> FxHashMap<InsChunkIdx, Vec<InsChunkIdx>> {
  mapped
    .iter()
    .map(|&idx| {
      let chunk = &chunk_graph.chunk_table[index_instantiated_chunks[idx].originate_from];
      let deps = chunk
        .cross_chunk_imports
        .iter()
        .flat_map(|importee| index_chunk_to_instances[*importee].iter().copied())
        .collect();
      (idx, deps)
    })
    .collect()
}

/// Replaces every edge to a mapped chunk with edges to the hashed chunks statically behind
/// it, following chains of mapped chunks. The result is what Phase 2 walks: mapped chunks
/// never appear in any closure, and everything their static imports reach does.
pub fn collapse_mapped_dependencies(
  index_direct_dependencies: &IndexVec<InsChunkIdx, Vec<InsChunkIdx>>,
  mapped: &FxHashSet<InsChunkIdx>,
  static_deps_of_mapped: &FxHashMap<InsChunkIdx, Vec<InsChunkIdx>>,
) -> IndexVec<InsChunkIdx, Vec<InsChunkIdx>> {
  fn behind(
    idx: InsChunkIdx,
    mapped: &FxHashSet<InsChunkIdx>,
    static_deps_of_mapped: &FxHashMap<InsChunkIdx, Vec<InsChunkIdx>>,
    visited_mapped: &mut FxHashSet<InsChunkIdx>,
    out: &mut FxIndexSet<InsChunkIdx>,
  ) {
    for dep in static_deps_of_mapped.get(&idx).map_or(&[][..], Vec::as_slice) {
      if mapped.contains(dep) {
        if visited_mapped.insert(*dep) {
          behind(*dep, mapped, static_deps_of_mapped, visited_mapped, out);
        }
      } else {
        out.insert(*dep);
      }
    }
  }

  index_direct_dependencies
    .iter_enumerated()
    .map(|(idx, deps)| {
      if !deps.iter().any(|dep| mapped.contains(dep)) {
        return deps.clone();
      }
      let mut out = FxIndexSet::default();
      let mut visited_mapped = FxHashSet::default();
      visited_mapped.insert(idx);
      for dep in deps {
        if mapped.contains(dep) {
          if visited_mapped.insert(*dep) {
            behind(*dep, mapped, static_deps_of_mapped, &mut visited_mapped, &mut out);
          }
        } else {
          out.insert(*dep);
        }
      }
      out.into_iter().collect()
    })
    .collect::<Vec<_>>()
    .into()
}
