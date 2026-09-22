# tidepool-effect — generic effect dispatch machinery

**Charter.** Belongs: `EffectHandler`/`DispatchEffect` traits and
HList-based handler-stack composition for dispatching algebraic effects at
runtime. Does NOT belong: concrete effect/verb definitions
(`tidepool-mcp`/`tidepool-protocol`), concrete handler implementations
(`tidepool-handlers`).
