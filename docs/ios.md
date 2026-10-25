# iOS and Apple signing

## Local Wallet on the Mac

`roofer up` builds and opens an isolated local Wallet app (needs Xcode and `xcodegen`; skip
with `--no-wallet`). It is signed with your Xcode team when one is signed in, so Touch ID
unlock works; otherwise it falls back to PIN only. Choose the team explicitly with
`PAYROOF_APPLE_TEAM`. Force a rebuild with `--rebuild-wallet`.

## Wallet on an iPhone

```sh
roofer ios [--device NAME] [--local] [--no-build]
roofer up --ios            # start the stack, then `roofer ios --local`
```

- Without `--local` the app is HTTPS-only (real networks only).
- With `--local` a separate test app (bundle ID `<id>.local`) is installed, preset to this
  Mac's Wi-Fi address and talking plain HTTP (private addresses only). Set `PAYROOF_LAN_IP`
  if the address is not detected, and do not set `PAYROOF_BIND_HOST=127.0.0.1`.

Once per iPhone:

1. Pair it with Xcode (cable, Trust) and turn on Developer Mode.
2. After the first install, trust the developer under Settings → General → VPN & Device
   Management.
3. Allow "Local Network" when the app asks.
4. Anvil starts off in the Wallet: turn it on in Networks & tokens.

Keep the Mac's address fixed (a DHCP reservation on the router): it is built into the
iPhone app and kept in projects created from it. The iPhone and the Mac must be on the same
Wi-Fi. Storefronts are `http://<slug>.<suffix>`, which only the Mac resolves: open them
there. macOS may ask to allow incoming connections for `payroof` and `stortop`.

If no team is found, sign in under Xcode → Settings → Accounts or set `PAYROOF_APPLE_TEAM`.
