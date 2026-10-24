//! `experimental.chunkImportMap.include` maps only the selected chunks, and a mapped chunk
//! is transparent for hashing: importers do not depend on its content but do depend on the
//! hashed chunks behind it.
//!
//! Graph (every variant): `main` → `page` → `import('./editor')` → `common` → `vendor`,
//! `editor` → `dist`, and `page` → `import('./slice')` → `zed`.
//! `main`, `common` and `slice` are mapped.
//!
//! - `b`: `vendor` changes (behind the mapped `common`)
//! - `c`: `common` changes, its imports do not
//! - `d`: `zed` changes (behind the dynamically imported mapped `slice`)
//! - `e1`/`e2`: `common` dynamically imports `lazy`, and `lazy` changes

use std::{path::PathBuf, sync::Arc};

use rolldown::{
  Bundler, BundlerOptions, ChunkImportMap, CodeSplittingMode, ExperimentalOptions, InputItem,
  ManualCodeSplittingOptions, MatchGroup, MatchGroupName, MatchGroupTest,
};
use rolldown_common::Output;
use rolldown_utils::js_regex::HybridRegex;
use rustc_hash::FxHashMap;

fn group(name: &str, pattern: &str) -> MatchGroup {
  MatchGroup {
    name: MatchGroupName::Static(name.to_string()),
    test: Some(MatchGroupTest::Regex(HybridRegex::new(pattern).unwrap())),
    min_size: Some(0.0),
    include_dependencies_recursively: Some(false),
    ..Default::default()
  }
}

async fn build(variant: &str) -> FxHashMap<String, String> {
  let cwd = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
    .join("tests/rolldown/function/chunk_import_map_include")
    .join(variant);
  let options = BundlerOptions {
    cwd: Some(cwd),
    input: Some(vec![InputItem {
      name: Some("main".to_string()),
      import: "./main.js".to_string(),
    }]),
    entry_filenames: Some("[name]-[hash:12].js".to_string().into()),
    chunk_filenames: Some("[name]-[hash:12].js".to_string().into()),
    code_splitting: Some(CodeSplittingMode::Advanced(ManualCodeSplittingOptions {
      min_size: Some(0.0),
      groups: Some(vec![
        group("common", "shared"),
        group("vendor", "vendor"),
        group("slice", "slice"),
        group("zed", "zed"),
        group("dist", "dist"),
        group("page", "page"),
        group("lazy", "lazy"),
      ]),
      ..Default::default()
    })),
    experimental: Some(ExperimentalOptions {
      chunk_import_map: Some(ChunkImportMap {
        include: Some(Arc::new(|chunk| {
          let mapped = matches!(chunk.name.as_str(), "main" | "common" | "slice");
          Box::pin(async move { Ok(mapped) })
        })),
        ..Default::default()
      }),
      ..Default::default()
    }),
    ..Default::default()
  };
  let mut bundler = Bundler::new(options).unwrap();
  let output = bundler.generate().await.unwrap();
  output
    .assets
    .into_iter()
    .map(|output| match output {
      Output::Chunk(chunk) => (chunk.filename.to_string(), chunk.code.clone()),
      Output::Asset(asset) => {
        (asset.filename.to_string(), String::from_utf8_lossy(asset.source.as_bytes()).into_owned())
      }
    })
    .collect()
}

fn file<'a>(output: &'a FxHashMap<String, String>, prefix: &str) -> &'a str {
  output
    .keys()
    .find(|name| name.starts_with(prefix) && name.ends_with(".js"))
    .unwrap_or_else(|| panic!("no {prefix}* in {:?}", output.keys().collect::<Vec<_>>()))
}

fn import_map(output: &FxHashMap<String, String>) -> Vec<(String, String)> {
  let value: serde_json::Value = serde_json::from_str(&output["importmap.json"]).unwrap();
  let mut imports = value["imports"]
    .as_object()
    .unwrap()
    .iter()
    .map(|(stable_name, target)| (stable_name.clone(), target.as_str().unwrap().to_string()))
    .collect::<Vec<_>>();
  imports.sort();
  imports
}

#[tokio::test(flavor = "multi_thread")]
async fn mapped_chunks_are_transparent_for_hashing() {
  let a = build("a").await;
  let b = build("b").await;
  let c = build("c").await;
  let d = build("d").await;

  // The import map lists exactly the included chunks, stable name → content-hashed name.
  let imports = import_map(&a);
  assert_eq!(imports.len(), 3, "{imports:?}");
  for (stable_name, target) in &imports {
    assert!(stable_name.starts_with('/') && target.starts_with('/'), "{stable_name} → {target}");
    assert_ne!(stable_name, target);
    assert!(a.contains_key(&target[1..]), "{target} is not an emitted file");
  }
  let common_stable_name =
    &imports.iter().find(|(stable_name, _)| stable_name.contains("/common-")).unwrap().0[1..];

  // Importers reference a mapped chunk by its stable name, never by its content-hashed name.
  let page = &a[file(&a, "page-")];
  assert!(page.contains(common_stable_name), "{page}");
  assert!(!page.contains(file(&a, "common-")), "{page}");
  assert!(!page.contains("!~{"), "unresolved placeholder: {page}");

  // b: `vendor`, reached only through the mapped `common`, changed. Its name is in
  // `page`'s dependency closure, so `page` is renamed; the stable names are not.
  assert_ne!(file(&a, "vendor-"), file(&b, "vendor-"));
  assert_ne!(file(&a, "page-"), file(&b, "page-"));
  assert_eq!(
    import_map(&a).into_iter().map(|(stable_name, _)| stable_name).collect::<Vec<_>>(),
    import_map(&b).into_iter().map(|(stable_name, _)| stable_name).collect::<Vec<_>>(),
  );

  // c: the mapped `common` changed but imports the same chunks. Its own file is renamed and
  // the import map follows; nothing that imports it is renamed.
  assert_ne!(file(&a, "common-"), file(&c, "common-"));
  assert_eq!(file(&a, "page-"), file(&c, "page-"));
  assert_eq!(file(&a, "editor-"), file(&c, "editor-"));
  assert_eq!(a[file(&a, "page-")], c[file(&c, "page-")]);
  assert_ne!(import_map(&a), import_map(&c));

  // d: `zed`, reached only through the dynamically imported mapped `slice`,
  // changed. `page` is renamed.
  assert_ne!(file(&a, "zed-"), file(&d, "zed-"));
  assert_ne!(file(&a, "page-"), file(&d, "page-"));
  assert_eq!(file(&a, "common-"), file(&d, "common-"));
}

#[tokio::test(flavor = "multi_thread")]
async fn dynamic_imports_of_mapped_chunks_do_not_reach_importers() {
  let e1 = build("e1").await;
  let e2 = build("e2").await;

  // `lazy` is only reached through the mapped `common`'s own dynamic import, which `common`
  // resolves from its content. Nothing an importer embeds changes, so nothing is renamed but
  // `lazy` and `common` themselves.
  assert_ne!(file(&e1, "lazy-"), file(&e2, "lazy-"));
  assert_ne!(file(&e1, "common-"), file(&e2, "common-"));
  assert_eq!(file(&e1, "page-"), file(&e2, "page-"));
  assert_eq!(file(&e1, "editor-"), file(&e2, "editor-"));
  assert_eq!(a_js_names(&e1), a_js_names(&e2));
}

fn a_js_names(output: &FxHashMap<String, String>) -> Vec<&String> {
  let mut names = output
    .keys()
    .filter(|name| {
      name.ends_with(".js") && !name.starts_with("lazy-") && !name.starts_with("common-")
    })
    .collect::<Vec<_>>();
  names.sort();
  names
}

#[tokio::test(flavor = "multi_thread")]
async fn stable_names_are_the_same_on_every_build() {
  assert_eq!(import_map(&build("a").await), import_map(&build("a").await));
}
