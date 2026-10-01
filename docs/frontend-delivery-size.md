# Frontend delivery and package-size checklist

The release path already uses Trunk's `wasm-release` profile (`opt-level = "z"`),
wasm-opt stripping, a boot skeleton, fingerprinted assets, and precompression.
Measure before optimizing: `scripts/check-web-size.mjs` records raw/Brotli bytes
for the actual shell dependency set and enforces the budget in CI.

## Keep these invariants

- Run wasm-opt through Trunk, not as a post-build rewrite: Trunk's SRI hashes must
  describe the final bytes. Keep `--strip-debug --strip-producers` and verify panic
  hooks and reader responsiveness when changing optimization settings.
- Serve `yomu-web-compressed` on the server; embed plain `yomu-web` in Tauri.
  Brotli/gzip siblings are generated at build time, not per HTTP request. Missing
  siblings must fall back to identity files. Include `Vary: accept-encoding` without
  replacing other Vary values.
- Fingerprint lengths vary (Trunk can drop leading zeroes). The static service
  that actually serves the SPA fallback must set revalidation headers, including
  on 304s: an asset-shaped URL is not proof that its response is an asset.
- The boot skeleton must be removed after mount, with reduced-motion support.
- Service Worker shell updates must cache all referenced assets before publishing
  the new shell. Test a real version transition followed by offline deep-link boot.
  Lazily loaded assets need an explicit offline policy.
- Compute stored-body sizes from bytes, not cached Content-Length: the browser
  decodes compressed bodies while retained headers can describe the wire payload.

## Lazy PDF reader

PDF.js is a pinned runtime dependency, copied by `scripts/prepare-reader-assets.mjs`
into ignored build assets. Local Trunk builds require `npm ci`; Nix supplies the
same version from a fixed-output npm archive, without network access during the
web build. Keep the upstream LICENSE and bundled font/data notices intact.

The viewer, PDF.js worker, fonts, CMaps, ICC profiles and WASM decoders load only
when opening a PDF: they are not part of the shell boot budget. Do not eagerly
import them from the SPA entrypoint. Precompression includes `.mjs` alongside
other scripts. Book resources and the viewer are network-only in the Service
Worker; EPUB/PDF device saves and their offline dependency closure are not yet
implemented. Changing that policy requires real worker-upgrade/offline tests,
not just caching the manifest.

## Native packages

- Preserve Nix's frame-pointer flags when adding panic-path remapping. Embedded
  toolchain paths can accidentally retain gigabytes in a runtime closure.
- Do not install Android's static library in the desktop package.
- Scope Android stripping to its target, not global RUSTFLAGS (which reaches the
  nested WASM build and can remove required target features). Verify the APK's
  shipped `.so`, not just build settings; `just apk` rejects a retained `.symtab`.
- Inject the workspace version into the Android build and remove stale generated
  tauri.properties first. Tauri's desktop Cargo-version fallback is not sufficient
  for Android. Verify the signed artifact before attaching it to a release.

Historical measurements and the implementation walkthroughs remain in Git. Do not
use those numbers as today's baseline; dependencies and UI features change it.
