use std::path::Path;
use serde_json::json;

fn prelude_path() -> std::path::PathBuf {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    manifest.parent().unwrap().join("haskell").join("lib")
}

fn run_plain(body: &str) -> serde_json::Value {
    let src = format!(
        r#"{{-# LANGUAGE NoImplicitPrelude, OverloadedStrings, PartialTypeSignatures #-}}
module Test where
import Tidepool.Prelude
import qualified Data.Text as T
import qualified Data.Map.Strict as Map
import qualified Data.Set as Set
default (Int, Text)

result :: _
result = {body}
"#
    );
    let pp = prelude_path();
    let include = [pp.as_path()];
    let val = tidepool_runtime::compile_and_run_pure(&src, "result", &include)
        .expect("compile_and_run_pure failed");
    val.to_json()
}

// --- List operations ---

#[test]
fn test_reverse() {
    assert_eq!(run_plain(r#"reverse [1,2,3 :: Int]"#), json!([3,2,1]));
}

#[test]
fn test_sort() {
    assert_eq!(run_plain(r#"sort [3,1,2 :: Int]"#), json!([1,2,3]));
}

#[test]
fn test_drop() {
    assert_eq!(run_plain(r#"drop 2 [1,2,3,4 :: Int]"#), json!([3,4]));
}

#[test]
fn test_head() {
    assert_eq!(run_plain(r#"head [1,2,3 :: Int]"#), json!(1));
}

#[test]
fn test_tail() {
    assert_eq!(run_plain(r#"tail [1,2,3 :: Int]"#), json!([2,3]));
}

#[test]
fn test_last() {
    assert_eq!(run_plain(r#"last [1,2,3 :: Int]"#), json!(3));
}

#[test]
fn test_init() {
    assert_eq!(run_plain(r#"init [1,2,3 :: Int]"#), json!([1,2]));
}

#[test]
fn test_sum() {
    assert_eq!(run_plain(r#"sum [1,2,3,4 :: Int]"#), json!(10));
}

#[test]
fn test_product() {
    assert_eq!(run_plain(r#"product [1,2,3,4 :: Int]"#), json!(24));
}

#[test]
fn test_minimum() {
    assert_eq!(run_plain(r#"minimum [3,1,2 :: Int]"#), json!(1));
}

#[test]
fn test_maximum() {
    assert_eq!(run_plain(r#"maximum [3,1,2 :: Int]"#), json!(3));
}

#[test]
fn test_foldl_prime() {
    assert_eq!(run_plain(r#"foldl' (+) (0 :: Int) [1,2,3]"#), json!(6));
}

#[test]
fn test_elem() {
    assert_eq!(run_plain(r#"elem (2 :: Int) [1,2,3]"#), json!(true));
}

#[test]
fn test_partition() {
    assert_eq!(run_plain(r#"partition even [1,2,3,4,5 :: Int]"#), json!([[2,4],[1,3,5]]));
}

#[test]
fn test_find() {
    assert_eq!(run_plain(r#"find even [1,2,3,4 :: Int]"#), json!(2));
}

// --- Prelude Text operations ---

#[test]
fn test_to_upper() {
    assert_eq!(run_plain(r#"T.toUpper "hello""#), json!("HELLO"));
}

#[test]
fn test_to_lower() {
    assert_eq!(run_plain(r#"T.toLower "HELLO""#), json!("hello"));
}

#[test]
fn test_strip() {
    assert_eq!(run_plain(r#"T.strip "  hello  ""#), json!("hello"));
}

#[test]
fn test_split_on() {
    assert_eq!(run_plain(r#"T.splitOn "," "a,b,c""#), json!(["a","b","c"]));
}

#[test]
fn test_replace() {
    assert_eq!(run_plain(r#"T.replace "old" "new" "the old way""#), json!("the new way"));
}

#[test]
fn test_is_prefix_of() {
    assert_eq!(run_plain(r#"T.isPrefixOf "hel" "hello""#), json!(true));
}

#[test]
fn test_is_suffix_of() {
    assert_eq!(run_plain(r#"T.isSuffixOf "llo" "hello""#), json!(true));
}

#[test]
fn test_is_infix_of() {
    assert_eq!(run_plain(r#"T.isInfixOf "ell" "hello""#), json!(true));
}

#[test]
fn test_words() {
    assert_eq!(run_plain(r#"T.words "hello world foo""#), json!(["hello","world","foo"]));
}

#[test]
fn test_lines() {
    assert_eq!(run_plain(r#"T.lines "a\nb\nc""#), json!(["a","b","c"]));
}

// --- Parsing ---

#[test]
fn test_parse_int() {
    assert_eq!(run_plain(r#"parseInt "42""#), json!(42));
}

#[test]
fn test_parse_int_neg() {
    assert_eq!(run_plain(r#"parseInt "-7""#), json!(-7));
}

#[test]
fn test_parse_double() {
    assert_eq!(run_plain(r#"parseDouble "3.14""#), json!(3.14));
}

#[test]
fn test_parse_int_m_fail() {
    assert_eq!(run_plain(r#"parseIntM "abc""#), json!(null));
}

// --- Map operations ---

#[test]
fn test_map_from_list() {
    assert_eq!(run_plain(r#"toJSON (Map.fromList [("a" :: Text, 1 :: Int), ("b" :: Text, 2 :: Int)])"#), json!({"a":1,"b":2}));
}

#[test]
fn test_map_insert() {
    assert_eq!(run_plain(r#"toJSON (Map.insert ("c" :: Text) (3 :: Int) (Map.fromList [("a" :: Text, 1 :: Int), ("b" :: Text, 2 :: Int)]))"#), json!({"a":1,"b":2,"c":3}));
}

#[test]
fn test_map_delete() {
    assert_eq!(run_plain(r#"toJSON (Map.delete ("a" :: Text) (Map.fromList [("a" :: Text, 1 :: Int), ("b" :: Text, 2 :: Int)]))"#), json!({"b":2}));
}

#[test]
fn test_map_size() {
    assert_eq!(run_plain(r#"Map.size (Map.fromList [("a" :: Text, 1 :: Int), ("b" :: Text, 2 :: Int)])"#), json!(2));
}

#[test]
fn test_map_find_with_default() {
    assert_eq!(run_plain(r#"Map.findWithDefault (0 :: Int) ("x" :: Text) (Map.fromList [("a" :: Text, 1 :: Int)])"#), json!(0));
}

// --- Set operations ---

#[test]
fn test_set_from_list() {
    assert_eq!(run_plain(r#"toJSON (Set.fromList [1,2,3 :: Int])"#), json!([1,2,3]));
}

#[test]
fn test_set_insert() {
    assert_eq!(run_plain(r#"toJSON (Set.insert (4 :: Int) (Set.fromList [1,2,3 :: Int]))"#), json!([1,2,3,4]));
}

#[test]
fn test_set_delete() {
    assert_eq!(run_plain(r#"toJSON (Set.delete (2 :: Int) (Set.fromList [1,2,3 :: Int]))"#), json!([1,3]));
}

#[test]
fn test_set_member() {
    assert_eq!(run_plain(r#"Set.member (2 :: Int) (Set.fromList [1,2,3 :: Int])"#), json!(true));
}

#[test]
fn test_set_size() {
    assert_eq!(run_plain(r#"Set.size (Set.fromList [1,2,3 :: Int])"#), json!(3));
}
