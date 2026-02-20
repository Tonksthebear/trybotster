# Push Subscription Migration: Per-Hub → Device-Level

After deploying the updated CLI, push subscriptions are stored at the device level
(`~/.config/botster/push_subscriptions.enc`) instead of per-hub
(`~/.config/botster/hubs/{hub_id}/push_subscriptions.enc`).

The old file is encrypted with the hub's key and the new location uses a different
key (`_device_push`), so you can't just copy the file. Easiest path:

## Steps

1. Deploy the updated CLI to the hub
2. Open the browser, go to Settings → Devices → your device
3. Click **Disable** on push notifications
4. Click **Enable** again
5. The browser re-subscribes and sends `push_sub` to the CLI, which writes it to the new device-level path with the new encryption key

That's it. The old per-hub file in `hubs/{hub_id}/push_subscriptions.enc` is now orphaned — delete it whenever:

```bash
rm ~/Library/Application\ Support/botster/hubs/2155a3d551b813e88c724201a8761023/push_subscriptions.enc
```
