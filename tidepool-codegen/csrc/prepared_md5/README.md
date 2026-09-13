# Prepared MD5 kernel provenance

`md5.c` and `md5.h` are copied from GHC tag `ghc-9.12.2-release`, peeled
commit `383be28ffdddf65b57b7b111bfc89808b4229ebc`:

- `libraries/ghc-internal/cbits/md5.c`
- `libraries/ghc-internal/include/md5.h`

The upstream SHA-256 digests are:

- `md5.c`: `2e5d9dff69905bdf1d8bacbc00f9fb75dc6a2559f0276794f3f36fc8f7857554`
- `md5.h`: `be128d81cad7364be65ce3b6d86f246cb4addfc0b7b7fe32b50af8876999cd15`

The implementation was written by Colin Plumb in 1993. Its source notice
places it in the public domain and claims no copyright.

The only local change is removal of `#include "HsFFI.h"` from `md5.c`. The
translation unit does not use anything from that header; removing it keeps the
Rust build independent of a GHC installation. No algorithm, type, symbol, or
other include was changed.
