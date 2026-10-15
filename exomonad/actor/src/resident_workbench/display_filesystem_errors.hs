shown <- do
  let errors = [FsNotFound "missing", FsNotUtf8 "encoding", FsSandbox "permission", FsBadRegex "regex", FsIo "io", FsDurabilityUnknown "durability", FsNonUtf8Path "path"]
  let nested = map (Right . Left :: FsError -> Either FsError (Either FsError Text)) errors
  display nested
