{-# LANGUAGE OverloadedLabels, OverloadedRecordDot #-}
import Tidepool.Effects.Core (Jev)

anchorTest :: Text
anchorTest = "assert_eq!(items, loaded);"

anchorNonTest :: Text
anchorNonTest = "pub fn save(items: &[Item], path: impl AsRef<Path>) -> Result<(), StoreError> {"

paddingAll :: [(Text, Text)]
paddingAll =
  [ ("p1", "pub enum StoreError {")
  , ("p2", "assert!(matches!(result, Err(StoreError::Json(_))));")
  , ("p3", "mod app;")
  , ("p4", "use crate::app::Item;")
  , ("p5", "Io(std::io::Error),")
  , ("p6", "StoreError::Io(e) => write!(f, \"io error: {e}\"),")
  , ("p7", "std::fs::write(path, contents)?;")
  , ("p8", "pub fn load(path: impl AsRef<Path>, limit: usize) -> Result<Vec<Item>, StoreError> {")
  , ("p9", "let dir = std::env::temp_dir();")
  , ("p10", "let mut f = std::fs::File::create(&path).expect(\"create temp file\");")
  , ("p11", "fn missing_file_returns_io_error() {")
  , ("p12", "fn parse_file_arg(mut args: impl Iterator<Item = String>) -> Option<String> {")
  , ("p13", "fn render_panel(panel: &dyn Panel, frame: &mut Frame, area: Rect, app: &App) {")
  , ("p14", "fn main() -> io::Result<()> {")
  , ("p15", "Some(path) => match store::load(path) {")
  , ("p16", "None => vec![")
  , ("p17", "let rows = Layout::default()")
  , ("p18", "if key.code == KeyCode::Char('q') {")
  , ("p19", "Action::Quit => {")
  , ("p20", "let args = vec![\"app\".into(), \"--file\".into(), \"todos.json\".into()];")
  , ("p21", "let args = vec![\"app\".into(), \"--file=todos.json\".into()];")
  , ("p22", "let args = vec![\"app\".into(), \"--other\".into()];")
  , ("p23", "let args = vec![\"app\".into(), \"--file\".into(), \"--other\".into()];")
  , ("p24", "impl From<std::io::Error> for StoreError {")
  , ("p25", "let items: Vec<Item> = serde_json::from_str(&contents)?;")
  , ("p26", "let path = dir.join(format!(\"tui-test-app-store-{}.json\", std::process::id()));")
  , ("p27", "let path = std::env::temp_dir().join(\"tui-test-app-store-does-not-exist.json\");")
  ]

runPoolSize :: Member Jev effs => Int -> Eff effs (Either J.JevError (Double, Double))
runPoolSize n = do
  let items = [("anchor_test", anchorTest), ("anchor_nontest", anchorNonTest)] ++ take (n - 2) paddingAll
      pool = J.pool #items [ (k, String v, k) | (k, v) <- items ]
      packet =
        #items := pool
          :& #each := J.eachIn pool (\ref ->
               #is_test := J.askAbout ref "Is this line of Rust source code located inside a `mod tests` block (a unit test module), rather than in the surrounding non-test code?"
                 :& Nil)
          :& Nil
  answer <- J.ask (J.state (object ["language" .= ("rust" :: Text)])) packet
  case answer of
    Left e -> pure (Left e)
    Right r -> do
      let a = J.answers r
          getYes k = case lookup k a.each of
            Just sub -> sub.is_test.yes
            Nothing -> -1
      pure (Right (getYes "anchor_test", getYes "anchor_nontest"))

report42 :: Either J.JevError (Double, Double) -> Value
report42 (Left e) = object ["error" .= T.pack (show e)]
report42 (Right (t, nt)) = object ["anchor_test_yes" .= t, "anchor_nontest_yes" .= nt]

do
  r5 <- runPoolSize 5
  r15 <- runPoolSize 15
  r30 <- runPoolSize 30
  pure (object
    [ "pool_5" .= report42 r5
    , "pool_15" .= report42 r15
    , "pool_30" .= report42 r30
    ])
