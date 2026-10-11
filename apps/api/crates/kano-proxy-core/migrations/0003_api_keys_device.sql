-- A CLI device can mint API keys for the coding agents it sets up (docs/cli.md
-- § Keys and groups). The key remembers the device, and revoking the device —
-- from the Access page, `kano-proxy logout`, or refresh-token reuse detection —
-- deletes the keys it minted by cascade. Keys created in the web UI keep NULL.
ALTER TABLE api_keys ADD COLUMN device_id TEXT REFERENCES cli_devices(id) ON DELETE CASCADE;
CREATE INDEX api_keys_device_id_idx ON api_keys(device_id);
