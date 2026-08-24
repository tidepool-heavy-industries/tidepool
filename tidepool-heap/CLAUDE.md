# tidepool-heap — heap object layout + copying-GC core

**Charter.** Belongs: `HeapObject` manual memory layout (raw byte buffers +
unsafe accessors) and the copying-GC core (Cheney scan, pointer-field
walking, `gc::raw` copy primitives). Does NOT belong: the nursery, frame
walker, and collection driver that use these primitives (`tidepool-codegen`),
`CoreExpr` IR types (`tidepool-repr`).
