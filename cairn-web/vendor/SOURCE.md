# Vendored dependency: `@bytecodealliance/wrpc`

`bytecodealliance-wrpc-0.1.0.tgz` is the official wRPC JavaScript SDK, produced
by running `npm pack` in the `js/` subdirectory of the wrpc repository.

- **Package:** `@bytecodealliance/wrpc@0.1.0`
- **Source repo:** https://github.com/bytecodealliance/wrpc (`js/` subdirectory)
- **Pinned commit:** `5a92c5c29e47d918ff15fb76455427fbcbddd423`

## Why vendored

npm cannot install a git dependency that lives in a *subdirectory* of a repo,
and the package is not yet published to npm. Until it is published, we commit
the packed tarball and reference it from `package.json` as a `file:` dependency.

This commit **must** match the wrpc commit the daemon's `wrpc-websockets` git
dependency pins, so the JS wire codec and the Rust frame codec are the same
version. If you re-pack, update the commit above and re-verify the daemon pin.

## How to refresh

```sh
git -C ../wrpc rev-parse HEAD        # must equal the pinned commit above
cd ../wrpc/js && npm pack --pack-destination /path/to/cairn-web/vendor
```

Revisit once `@bytecodealliance/wrpc` is published to npm — then this vendor
directory and the `file:` dependency can be replaced with a normal version pin.
