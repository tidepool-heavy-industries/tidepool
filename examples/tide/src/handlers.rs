use std::collections::HashMap;
use std::path::PathBuf;
use thiserror::Error;
use tracing::{debug, error, info, warn};
use url::Url;

use tidepool_bridge::ToCore;
use tidepool_bridge_derive::FromCore;
use tidepool_effect::{EffectContext, EffectError, EffectHandler};
use tidepool_eval::value::Value;

/// Structured errors for Tide effect handlers.
#[derive(Error, Debug)]
pub enum TideError {
    #[error("Parse error: {0}")]
    Parse(String),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("HTTP error: {0}")]
    Http(String),
    #[error("Internal error: {0}")]
    Internal(String),
}

impl From<TideError> for EffectError {
    fn from(e: TideError) -> Self {
        EffectError::Handler(e.to_string())
    }
}

/// Parse a user-provided string into a URL, prepending `https://` if no scheme is given.
fn parse_url(raw: &str) -> Result<Url, TideError> {
    Url::parse(raw).or_else(|_| Url::parse(&format!("https://{}", raw)))
        .map_err(|e| TideError::Http(format!("invalid URL '{}': {}", raw, e)))
}

// === Tag 0: Repl ===

#[derive(FromCore)]
pub enum ReplReq {
    #[core(name = "ReadLine")]
    ReadLine,
    #[core(name = "Display")]
    Display(String),
}

enum InputSource {
    Interactive(rustyline::DefaultEditor),
    File { lines: Vec<String>, pos: usize },
}

pub struct ReplHandler {
    source: InputSource,
}

impl ReplHandler {
    pub fn new() -> anyhow::Result<Self> {
        Ok(ReplHandler {
            source: InputSource::Interactive(rustyline::DefaultEditor::new()?),
        })
    }

    pub fn from_file(path: &str) -> anyhow::Result<Self> {
        let contents = std::fs::read_to_string(path)?;
        let lines: Vec<String> = contents
            .lines()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .collect();
        Ok(ReplHandler {
            source: InputSource::File { lines, pos: 0 },
        })
    }
}

fn parse_and_serialize(input: &str, cx: &EffectContext) -> Result<Value, TideError> {
    let expr = crate::parser::parse(input)
        .map_err(|e| TideError::Parse(format!("{:?}", e)))?;
    expr.to_value(cx.table())
        .map_err(|e| TideError::Internal(format!("ToCore failed: {:?}", e)))
}

fn read_interactive(
    editor: &mut rustyline::DefaultEditor,
    cx: &EffectContext,
) -> Result<Option<Value>, TideError> {
    loop {
        match editor.readline("tide> ") {
            Ok(line) => {
                let t = line.trim().to_string();
                if t.is_empty() {
                    continue;
                }
                if t.starts_with('/') {
                    match t.as_str() {
                        "/help" => {
                            println!("\
Tide expression language

  Literals:     42  \"hello\"  true  false  [1, 2, 3]
  Arithmetic:   +  -  *  /  (unary -)
  Comparison:   ==  !=  <  >  <=  >=
  Concatenation: \"a\" ++ \"b\"
  Variables:    let x = 5        (bind and echo)
                let x = 5; x+1  (bind with body)
  Conditionals: if x > 0 then x else -x
  Lambdas:      \\x y -> x + y
  Function call: f(1, 2)
  Builtins:     print(v)  len(xs)  str(v)  int(v)
                concat(a, b)  fetch(url)
                read_file(path)  write_file(path, s)

  /help   Show this message
  /exit   Quit the REPL");
                            continue;
                        }
                        "/exit" => return Ok(None),
                        _ => {
                            warn!("Unknown command: {}", t);
                            continue;
                        }
                    }
                }
                editor.add_history_entry(&t).ok();
                
                // Try parsing with miette for fancy errors
                match crate::parser::parse(&t) {
                    Ok(expr) => {
                        match expr.to_value(cx.table()) {
                            Ok(val) => return Ok(Some(val)),
                            Err(e) => return Err(TideError::Internal(format!("ToCore failed: {:?}", e))),
                        }
                    }
                    Err(e) => {
                        // Display fancy miette error
                        eprintln!("{:?}", e);
                        // loop: re-prompt
                    }
                }
            }
            Err(_) => return Ok(None),
        }
    }
}

fn read_file_line(
    lines: &[String],
    pos: &mut usize,
    cx: &EffectContext,
) -> Result<Option<Value>, TideError> {
    if *pos < lines.len() {
        let text = lines[*pos].clone();
        *pos += 1;
        debug!("Processing file line {}: {}", pos, text);
        match crate::parser::parse(&text) {
            Ok(expr) => {
                match expr.to_value(cx.table()) {
                    Ok(val) => Ok(Some(val)),
                    Err(e) => Err(TideError::Internal(format!("ToCore failed: {:?}", e))),
                }
            }
            Err(e) => {
                error!("{:?}", e);
                Ok(None)
            }
        }
    } else {
        Ok(None)
    }
}

impl EffectHandler for ReplHandler {
    type Request = ReplReq;

    fn handle(&mut self, req: ReplReq, cx: &EffectContext) -> Result<Value, EffectError> {
        match req {
            ReplReq::ReadLine => {
                let result = match &mut self.source {
                    InputSource::Interactive(editor) => read_interactive(editor, cx)?,
                    InputSource::File { lines, pos } => read_file_line(lines, pos, cx)?,
                };
                cx.respond(result)
            }
            ReplReq::Display(s) => {
                info!("Tide: {}", s);
                println!("{}", s);
                cx.respond(())
            }
        }
    }
}

// === Tag 1: Console ===

#[derive(FromCore)]
pub enum ConsoleReq {
    #[core(name = "Print")]
    Print(String),
}

pub struct ConsoleHandler;

impl EffectHandler for ConsoleHandler {
    type Request = ConsoleReq;

    fn handle(&mut self, req: ConsoleReq, cx: &EffectContext) -> Result<Value, EffectError> {
        match req {
            ConsoleReq::Print(s) => {
                info!("Console: {}", s);
                println!("{}", s);
                cx.respond(())
            }
        }
    }
}

// === Tag 2: Env ===

#[derive(FromCore)]
pub enum EnvReq {
    #[core(name = "EnvLookup")]
    EnvLookup(String),
    #[core(name = "EnvExtend")]
    EnvExtend(String, Value),
    #[core(name = "EnvRemove")]
    EnvRemove(String),
    #[core(name = "EnvSnapshot")]
    EnvSnapshot,
}

pub struct EnvHandler {
    env: HashMap<String, Value>,
}

impl EnvHandler {
    pub fn new() -> Self {
        EnvHandler {
            env: HashMap::new(),
        }
    }
}

impl EffectHandler for EnvHandler {
    type Request = EnvReq;

    fn handle(&mut self, req: EnvReq, cx: &EffectContext) -> Result<Value, EffectError> {
        match req {
            EnvReq::EnvLookup(key) => {
                debug!("Lookup: {}", key);
                let result = self.env.get(&key).cloned();
                cx.respond(result)
            }
            EnvReq::EnvExtend(key, val) => {
                debug!("Extend: {}", key);
                self.env.insert(key, val);
                cx.respond(())
            }
            EnvReq::EnvRemove(key) => {
                debug!("Remove: {}", key);
                self.env.remove(&key);
                cx.respond(())
            }
            EnvReq::EnvSnapshot => {
                debug!("Snapshot: {} bindings", self.env.len());
                let pairs: Vec<(String, Value)> = self.env.iter()
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect();
                cx.respond(pairs)
            }
        }
    }
}

// === Tag 3: Net ===

#[derive(FromCore)]
pub enum NetReq {
    #[core(name = "HttpGet")]
    HttpGet(String),
}

pub struct NetHandler;

impl EffectHandler for NetHandler {
    type Request = NetReq;

    fn handle(&mut self, req: NetReq, cx: &EffectContext) -> Result<Value, EffectError> {
        match req {
            NetReq::HttpGet(raw) => {
                let url = parse_url(&raw)?;
                info!("GET {}", url);
                let body = ureq::get(url.as_str())
                    .call()
                    .map_err(|e| TideError::Http(e.to_string()))?
                    .into_string()
                    .map_err(|e| TideError::Http(e.to_string()))?;
                cx.respond(body)
            }
        }
    }
}

// === Tag 4: Fs ===

#[derive(FromCore)]
pub enum FsReq {
    #[core(name = "FsRead")]
    FsRead(String),
    #[core(name = "FsWrite")]
    FsWrite(String, String),
}

pub struct FsHandler;

impl EffectHandler for FsHandler {
    type Request = FsReq;

    fn handle(&mut self, req: FsReq, cx: &EffectContext) -> Result<Value, EffectError> {
        match req {
            FsReq::FsRead(path) => {
                let path = PathBuf::from(path);
                info!("Reading file: {}", path.display());
                let contents = std::fs::read_to_string(&path).map_err(TideError::from)?;
                cx.respond(contents)
            }
            FsReq::FsWrite(path, contents) => {
                let path = PathBuf::from(path);
                info!("Writing file: {}", path.display());
                std::fs::write(&path, &contents).map_err(TideError::from)?;
                cx.respond(())
            }
        }
    }
}
