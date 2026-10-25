# Troubleshooting

- **"not inside the Payroof workspace"**: `cd` into the workspace (the directory holding
  `payroof/`, `stor/` and `wallet/`) or set `ROOFER_ROOT`.
- **"anvil (Foundry) not found" / "solc not found"**: install [Foundry](https://getfoundry.sh)
  and `solc`; `roofer e2e` compiles its test tokens with `solc`.
- **"no Apple development team found"**: sign in under Xcode → Settings → Accounts, or set
  `PAYROOF_APPLE_TEAM`. Without a team the local Wallet uses PIN only.
- **`--local` needs the Mac's Wi-Fi IP**: set `PAYROOF_LAN_IP`, and do not set
  `PAYROOF_BIND_HOST=127.0.0.1`.
- **No new stores can be created**: no Google service-account key was found; set
  `STOR_GOOGLE_CREDENTIALS` (see [environment](environment.md)).
- **Wallet cannot see payments after a reset**: the Payroof database is gone; connect the
  Wallet again with a new project name.
- **macOS asks about incoming connections** for `payroof` and `stortop`: allow them for LAN
  access, or use `PAYROOF_BIND_HOST=127.0.0.1`.
- **Anything else**: `roofer status` and `roofer logs [anvil|payroof|stor]`; as a last resort
  `roofer reset --yes`, which deletes the chain, databases, stores, orders and tokens.
