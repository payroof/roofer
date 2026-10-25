# Workspace layout

`roofer` expects the projects as siblings in one workspace directory:

```
workspace/
├── payroof/   payment server
├── stor/      storefronts (stortop)
├── wallet/    Payroof Wallet (Rust core + Apple apps)
└── roofer/    this CLI and the end-to-end tests (e2e/)
```

- Install from a checkout: `cargo install --path roofer` (from the workspace root).
- `roofer` finds the workspace from the current directory (any directory inside it), else
  from the checkout it was built from; `ROOFER_ROOT=/path/to/workspace` overrides both.
- State, secrets and logs live in `.local/manual/` at the workspace root.
- Stor reaches Google Sheets with a service-account key: see
  [environment variables](environment.md) (`STOR_GOOGLE_CREDENTIALS`).
- The rust toolchain is pinned in [`rust-toolchain.toml`](../rust-toolchain.toml).
