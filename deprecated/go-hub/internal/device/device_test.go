package device

import (
	"crypto/ed25519"
	"crypto/rand"
	"encoding/base64"
	"encoding/json"
	"os"
	"path/filepath"
	"strings"
	"testing"
)

func setupTestDir(t *testing.T) string {
	t.Helper()
	dir := t.TempDir()
	t.Setenv("BOTSTER_CONFIG_DIR", dir)
	return dir
}

func TestComputeFingerprint(t *testing.T) {
	publicKey, _, err := ed25519.GenerateKey(rand.Reader)
	if err != nil {
		t.Fatalf("failed to generate key: %v", err)
	}

	fingerprint := ComputeFingerprint(publicKey)

	// Should be 8 hex bytes separated by colons
	parts := strings.Split(fingerprint, ":")
	if len(parts) != 8 {
		t.Errorf("expected 8 parts, got %d: %s", len(parts), fingerprint)
	}

	for _, part := range parts {
		if len(part) != 2 {
			t.Errorf("expected 2-char part, got %d: %s", len(part), part)
		}
		for _, c := range part {
			if !((c >= '0' && c <= '9') || (c >= 'a' && c <= 'f')) {
				t.Errorf("expected hex digit, got %c", c)
			}
		}
	}
}

func TestComputeFingerprintConsistent(t *testing.T) {
	publicKey, _, err := ed25519.GenerateKey(rand.Reader)
	if err != nil {
		t.Fatalf("failed to generate key: %v", err)
	}

	fp1 := ComputeFingerprint(publicKey)
	fp2 := ComputeFingerprint(publicKey)

	if fp1 != fp2 {
		t.Errorf("fingerprint not consistent: %s != %s", fp1, fp2)
	}
}

func TestComputeFingerprintDifferent(t *testing.T) {
	pub1, _, _ := ed25519.GenerateKey(rand.Reader)
	pub2, _, _ := ed25519.GenerateKey(rand.Reader)

	fp1 := ComputeFingerprint(pub1)
	fp2 := ComputeFingerprint(pub2)

	if fp1 == fp2 {
		t.Error("different keys should have different fingerprints")
	}
}

func TestLoadOrCreateNew(t *testing.T) {
	dir := setupTestDir(t)

	dev, err := LoadOrCreate()
	if err != nil {
		t.Fatalf("LoadOrCreate failed: %v", err)
	}

	if dev.Fingerprint == "" {
		t.Error("expected fingerprint")
	}
	if dev.Name == "" {
		t.Error("expected name")
	}
	if len(dev.SigningKey) != ed25519.PrivateKeySize {
		t.Errorf("unexpected signing key size: %d", len(dev.SigningKey))
	}
	if len(dev.VerifyingKey) != ed25519.PublicKeySize {
		t.Errorf("unexpected verifying key size: %d", len(dev.VerifyingKey))
	}

	// Config file should exist
	configPath := filepath.Join(dir, "device.json")
	if _, err := os.Stat(configPath); os.IsNotExist(err) {
		t.Error("device.json should exist")
	}

	// Signing key file should exist (test mode)
	keyPath := filepath.Join(dir, "device.signing_key")
	if _, err := os.Stat(keyPath); os.IsNotExist(err) {
		t.Error("signing key file should exist in test mode")
	}
}

func TestLoadOrCreateExisting(t *testing.T) {
	dir := setupTestDir(t)

	// Create initial device
	dev1, err := LoadOrCreate()
	if err != nil {
		t.Fatalf("first LoadOrCreate failed: %v", err)
	}

	// Load again - should return same identity
	dev2, err := LoadOrCreate()
	if err != nil {
		t.Fatalf("second LoadOrCreate failed: %v", err)
	}

	if dev1.Fingerprint != dev2.Fingerprint {
		t.Errorf("fingerprint mismatch: %s != %s", dev1.Fingerprint, dev2.Fingerprint)
	}
	if !dev1.VerifyingKey.Equal(dev2.VerifyingKey) {
		t.Error("verifying keys should match")
	}

	_ = dir // use dir
}

func TestDeviceSaveAndLoad(t *testing.T) {
	setupTestDir(t)

	dev, err := LoadOrCreate()
	if err != nil {
		t.Fatalf("LoadOrCreate failed: %v", err)
	}

	// Set device ID and save
	id := int64(12345)
	if err := dev.SetDeviceID(id); err != nil {
		t.Fatalf("SetDeviceID failed: %v", err)
	}

	// Reload and verify
	dev2, err := LoadOrCreate()
	if err != nil {
		t.Fatalf("reload failed: %v", err)
	}

	if dev2.DeviceID == nil {
		t.Error("expected device ID after reload")
	} else if *dev2.DeviceID != id {
		t.Errorf("device ID mismatch: got %d, want %d", *dev2.DeviceID, id)
	}
}

func TestClearDeviceID(t *testing.T) {
	setupTestDir(t)

	dev, err := LoadOrCreate()
	if err != nil {
		t.Fatalf("LoadOrCreate failed: %v", err)
	}

	// Set and then clear device ID
	if err := dev.SetDeviceID(999); err != nil {
		t.Fatalf("SetDeviceID failed: %v", err)
	}

	if err := dev.ClearDeviceID(); err != nil {
		t.Fatalf("ClearDeviceID failed: %v", err)
	}

	// Reload and verify cleared
	dev2, err := LoadOrCreate()
	if err != nil {
		t.Fatalf("reload failed: %v", err)
	}

	if dev2.DeviceID != nil {
		t.Errorf("expected nil device ID, got %d", *dev2.DeviceID)
	}
}

func TestClearDeviceIDNoop(t *testing.T) {
	setupTestDir(t)

	dev, err := LoadOrCreate()
	if err != nil {
		t.Fatalf("LoadOrCreate failed: %v", err)
	}

	// Clear when already nil should succeed
	if err := dev.ClearDeviceID(); err != nil {
		t.Errorf("ClearDeviceID on nil should succeed: %v", err)
	}
}

func TestVerifyingKeyBase64(t *testing.T) {
	setupTestDir(t)

	dev, err := LoadOrCreate()
	if err != nil {
		t.Fatalf("LoadOrCreate failed: %v", err)
	}

	b64 := dev.VerifyingKeyBase64()

	// Should be valid base64
	decoded, err := base64.StdEncoding.DecodeString(b64)
	if err != nil {
		t.Fatalf("invalid base64: %v", err)
	}

	if len(decoded) != ed25519.PublicKeySize {
		t.Errorf("unexpected decoded length: got %d, want %d", len(decoded), ed25519.PublicKeySize)
	}
}

func TestSignAndVerify(t *testing.T) {
	setupTestDir(t)

	dev, err := LoadOrCreate()
	if err != nil {
		t.Fatalf("LoadOrCreate failed: %v", err)
	}

	message := []byte("hello world")
	signature := dev.Sign(message)

	if !dev.Verify(message, signature) {
		t.Error("signature verification failed")
	}

	// Tampered message should fail
	if dev.Verify([]byte("hello worlX"), signature) {
		t.Error("tampered message should fail verification")
	}

	// Tampered signature should fail
	badSig := make([]byte, len(signature))
	copy(badSig, signature)
	badSig[0] ^= 0xff
	if dev.Verify(message, badSig) {
		t.Error("tampered signature should fail verification")
	}
}

func TestDefaultName(t *testing.T) {
	name := defaultName()

	if !strings.HasPrefix(name, "Botster CLI") {
		t.Errorf("expected name to start with 'Botster CLI', got: %s", name)
	}
}

func TestStoredDeviceJSON(t *testing.T) {
	stored := StoredDevice{
		VerifyingKey: "dGVzdGtleQ==",
		Fingerprint:  "aa:bb:cc:dd:ee:ff:00:11",
		Name:         "Test Device",
		DeviceID:     nil,
	}

	data, err := json.Marshal(stored)
	if err != nil {
		t.Fatalf("marshal failed: %v", err)
	}

	var parsed StoredDevice
	if err := json.Unmarshal(data, &parsed); err != nil {
		t.Fatalf("unmarshal failed: %v", err)
	}

	if parsed.VerifyingKey != stored.VerifyingKey {
		t.Error("verifying key mismatch")
	}
	if parsed.Fingerprint != stored.Fingerprint {
		t.Error("fingerprint mismatch")
	}
	if parsed.Name != stored.Name {
		t.Error("name mismatch")
	}
	if parsed.DeviceID != nil {
		t.Error("device ID should be nil")
	}
}

func TestStoredDeviceJSONWithID(t *testing.T) {
	id := int64(42)
	stored := StoredDevice{
		VerifyingKey: "dGVzdGtleQ==",
		Fingerprint:  "aa:bb:cc:dd:ee:ff:00:11",
		Name:         "Test Device",
		DeviceID:     &id,
	}

	data, err := json.Marshal(stored)
	if err != nil {
		t.Fatalf("marshal failed: %v", err)
	}

	var parsed StoredDevice
	if err := json.Unmarshal(data, &parsed); err != nil {
		t.Fatalf("unmarshal failed: %v", err)
	}

	if parsed.DeviceID == nil {
		t.Fatal("expected device ID")
	}
	if *parsed.DeviceID != id {
		t.Errorf("device ID mismatch: got %d, want %d", *parsed.DeviceID, id)
	}
}

func TestConfigPathCreatesDir(t *testing.T) {
	dir := t.TempDir()
	subdir := filepath.Join(dir, "nested", "config")
	t.Setenv("BOTSTER_CONFIG_DIR", subdir)

	_, err := LoadOrCreate()
	if err != nil {
		t.Fatalf("LoadOrCreate failed: %v", err)
	}

	// Directory should be created
	info, err := os.Stat(subdir)
	if err != nil {
		t.Fatalf("config dir not created: %v", err)
	}
	if !info.IsDir() {
		t.Error("expected directory")
	}
}

func TestFilePermissions(t *testing.T) {
	dir := setupTestDir(t)

	_, err := LoadOrCreate()
	if err != nil {
		t.Fatalf("LoadOrCreate failed: %v", err)
	}

	configPath := filepath.Join(dir, "device.json")
	info, err := os.Stat(configPath)
	if err != nil {
		t.Fatalf("stat failed: %v", err)
	}

	// On Unix, should be 0600
	mode := info.Mode().Perm()
	if mode != 0600 {
		t.Errorf("expected 0600 permissions, got %o", mode)
	}

	keyPath := filepath.Join(dir, "device.signing_key")
	info, err = os.Stat(keyPath)
	if err != nil {
		t.Fatalf("stat signing key failed: %v", err)
	}

	mode = info.Mode().Perm()
	if mode != 0600 {
		t.Errorf("expected 0600 permissions for signing key, got %o", mode)
	}
}

func TestCorruptedConfigFile(t *testing.T) {
	dir := setupTestDir(t)
	configPath := filepath.Join(dir, "device.json")

	// Write corrupted JSON
	if err := os.WriteFile(configPath, []byte("not json"), 0600); err != nil {
		t.Fatalf("write failed: %v", err)
	}

	_, err := LoadOrCreate()
	if err == nil {
		t.Error("expected error for corrupted config")
	}
}

func TestMissingSigningKey(t *testing.T) {
	dir := setupTestDir(t)

	// Create device first
	dev, err := LoadOrCreate()
	if err != nil {
		t.Fatalf("initial create failed: %v", err)
	}

	// Delete the signing key file
	keyPath := filepath.Join(dir, "device.signing_key")
	if err := os.Remove(keyPath); err != nil {
		t.Fatalf("remove key failed: %v", err)
	}

	// Reload should fail
	_, err = LoadOrCreate()
	if err == nil {
		t.Error("expected error when signing key is missing")
	}

	_ = dev // use dev
}

func TestGetters(t *testing.T) {
	setupTestDir(t)

	dev, err := LoadOrCreate()
	if err != nil {
		t.Fatalf("LoadOrCreate failed: %v", err)
	}

	// Test GetFingerprint
	fp := dev.GetFingerprint()
	if fp != dev.Fingerprint {
		t.Errorf("GetFingerprint mismatch: %s != %s", fp, dev.Fingerprint)
	}

	// Test GetName
	name := dev.GetName()
	if name != dev.Name {
		t.Errorf("GetName mismatch: %s != %s", name, dev.Name)
	}

	// Test GetDeviceID when nil
	id := dev.GetDeviceID()
	if id != nil {
		t.Error("expected nil device ID")
	}

	// Test GetDeviceID after setting
	if err := dev.SetDeviceID(100); err != nil {
		t.Fatalf("SetDeviceID failed: %v", err)
	}
	id = dev.GetDeviceID()
	if id == nil {
		t.Fatal("expected non-nil device ID")
	}
	if *id != 100 {
		t.Errorf("GetDeviceID mismatch: got %d, want 100", *id)
	}
}

func TestLoadOrCreateWithPath(t *testing.T) {
	dir := t.TempDir()
	// Don't set env var, use explicit path

	dev, err := LoadOrCreateWithPath(dir)
	if err != nil {
		t.Fatalf("LoadOrCreateWithPath failed: %v", err)
	}

	if dev.Fingerprint == "" {
		t.Error("expected fingerprint")
	}

	// Config file should exist in specified directory
	configPath := filepath.Join(dir, "device.json")
	if _, err := os.Stat(configPath); os.IsNotExist(err) {
		t.Error("device.json should exist in specified directory")
	}
}

func TestShouldSkipKeyring(t *testing.T) {
	// Clear env first
	os.Unsetenv("BOTSTER_SKIP_KEYRING")
	os.Unsetenv("BOTSTER_CONFIG_DIR")

	// Test BOTSTER_SKIP_KEYRING=1
	t.Setenv("BOTSTER_SKIP_KEYRING", "1")
	if !shouldSkipKeyring() {
		t.Error("expected skip with BOTSTER_SKIP_KEYRING=1")
	}

	// Test BOTSTER_SKIP_KEYRING=true
	t.Setenv("BOTSTER_SKIP_KEYRING", "true")
	if !shouldSkipKeyring() {
		t.Error("expected skip with BOTSTER_SKIP_KEYRING=true")
	}

	// Test BOTSTER_CONFIG_DIR triggers skip
	os.Unsetenv("BOTSTER_SKIP_KEYRING")
	t.Setenv("BOTSTER_CONFIG_DIR", "/tmp/test")
	if !shouldSkipKeyring() {
		t.Error("expected skip with BOTSTER_CONFIG_DIR set")
	}
}

func TestConcurrentAccess(t *testing.T) {
	setupTestDir(t)

	dev, err := LoadOrCreate()
	if err != nil {
		t.Fatalf("LoadOrCreate failed: %v", err)
	}

	// Concurrent reads should not panic
	done := make(chan bool, 10)
	for i := 0; i < 10; i++ {
		go func() {
			_ = dev.GetFingerprint()
			_ = dev.GetName()
			_ = dev.GetDeviceID()
			_ = dev.VerifyingKeyBase64()
			done <- true
		}()
	}

	for i := 0; i < 10; i++ {
		<-done
	}
}
