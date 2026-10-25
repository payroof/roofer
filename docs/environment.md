# Environment variables

| Variable | Meaning |
| --- | --- |
| `ROOFER_ROOT=DIR` | The workspace directory. Default: the closest directory containing `payroof/`, `stor/` and `wallet/`, searching upward from the current directory, then the checkout `roofer` was built from |
| `PAYROOF_DEFAULT_TOKENS="TEST:6 USDC:6 DAI:18"` | Tokens deployed on a fresh chain (`""` = none) |
| `PAYROOF_BIND_HOST=127.0.0.1` | Keep Anvil, Payroof and Stor on this Mac only (default `0.0.0.0`: LAN access) |
| `PAYROOF_LAN_IP=192.168.1.20` | The Mac's LAN address (default: the Wi-Fi/Ethernet IP) |
| `STOR_GOOGLE_CREDENTIALS=FILE` | Google service-account key; stores are Google Sheets. Default: the first `service_account` `*.json` in `.local/` or at the workspace root; none means no new stores |
| `PAYROOF_APPLE_TEAM` | Apple development team for Wallet signing (default: the first team signed in to Xcode) |
| `PAYROOF_IOS_DEVICE` | iPhone name (default: the only paired iPhone) |
| `PAYROOF_IOS_BUNDLE_ID` | Bundle ID used when signing the Wallet |
| `NO_COLOR` | Disable colored output |

While the stack runs with the default bind host, anyone on your network can reach these test
services, including the Anvil chain with its unlocked test accounts. Set
`PAYROOF_BIND_HOST=127.0.0.1` to keep them local. Never send real funds to the test
addresses or reuse their mnemonics.
