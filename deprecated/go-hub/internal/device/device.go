// Package device manages device identity for CLI authentication.
//
// This package handles:
// - Ed25519 signing keypair generation and persistence
// - Device registration with the Rails server
// - Fingerprint generation for visual verification
package device

import (
	"crypto/ed25519"
	"crypto/rand"
	"crypto/sha256"
	"encoding/base64"
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"sync"

	"github.com/zalando/go-keyring"
)

// Keyring configuration.
const (
	KeyringService       = "botster"
	KeyringSigningSuffix = "signing"
)

// StoredDevice represents the device identity stored in device.json.
// Secret keys are stored in OS keyring, not in this file.
type StoredDevice struct {
	// Base64-encoded Ed25519 public key.
	VerifyingKey string `json:"verifying_key"`
	// Human-readable fingerprint for visual verification.
	Fingerprint string `json:"fingerprint"`
	// Device name (e.g., "Botster CLI").
	Name string `json:"name"`
	// Server-assigned device ID (set after registration).
	DeviceID *int64 `json:"device_id,omitempty"`
}

// Device represents the runtime device identity with parsed keys.
type Device struct {
	// Ed25519 private key for signing.
	SigningKey ed25519.PrivateKey
	// Ed25519 public key.
	VerifyingKey ed25519.PublicKey
	// Human-readable fingerprint for verification.
	Fingerprint string
	// Device name.
	Name string
	// Server-assigned device ID after registration.
	DeviceID *int64
	// Path to the device config file.
	configPath string

	mu sync.RWMutex
}

// shouldSkipKeyring checks if keyring should be skipped (for testing).
func shouldSkipKeyring() bool {
	if v := os.Getenv("BOTSTER_SKIP_KEYRING"); v == "1" || strings.ToLower(v) == "true" {
		return true
	}
	// Auto-detect test mode: integration tests set BOTSTER_CONFIG_DIR
	_, hasConfigDir := os.LookupEnv("BOTSTER_CONFIG_DIR")
	return hasConfigDir
}

// LoadOrCreate loads existing device or creates a new one.
func LoadOrCreate() (*Device, error) {
	return LoadOrCreateWithPath("")
}

// LoadOrCreateWithPath loads existing device or creates a new one at the specified config directory.
func LoadOrCreateWithPath(configDir string) (*Device, error) {
	configPath, err := getConfigPath(configDir)
	if err != nil {
		return nil, err
	}

	if _, err := os.Stat(configPath); err == nil {
		return loadFromFile(configPath)
	}

	return createNew(configPath)
}

// getConfigPath returns the device config file path.
func getConfigPath(configDir string) (string, error) {
	if configDir == "" {
		configDir = os.Getenv("BOTSTER_CONFIG_DIR")
	}

	if configDir == "" {
		homeDir, err := os.UserHomeDir()
		if err != nil {
			return "", fmt.Errorf("could not determine home directory: %w", err)
		}
		configDir = filepath.Join(homeDir, ".config", "botster")
	}

	if err := os.MkdirAll(configDir, 0700); err != nil {
		return "", fmt.Errorf("failed to create config directory: %w", err)
	}

	return filepath.Join(configDir, "device.json"), nil
}

// signingKeyFilePath returns the path for file-based signing key storage.
func signingKeyFilePath(configPath string) string {
	return strings.TrimSuffix(configPath, ".json") + ".signing_key"
}

// storeSigningKey stores the signing key (keyring or file based on environment).
func storeSigningKey(configPath, fingerprint string, signingKey ed25519.PrivateKey) error {
	secretB64 := base64.StdEncoding.EncodeToString(signingKey.Seed())

	if shouldSkipKeyring() {
		// Test mode: store in file
		keyPath := signingKeyFilePath(configPath)
		if err := os.WriteFile(keyPath, []byte(secretB64), 0600); err != nil {
			return fmt.Errorf("failed to write signing key file: %w", err)
		}
		return nil
	}

	// Production: use OS keyring
	entryName := fmt.Sprintf("%s-%s", fingerprint, KeyringSigningSuffix)
	if err := keyring.Set(KeyringService, entryName, secretB64); err != nil {
		return fmt.Errorf("failed to store in keyring: %w", err)
	}

	return nil
}

// loadSigningKey loads the signing key (keyring or file based on environment).
func loadSigningKey(configPath, fingerprint string) (ed25519.PrivateKey, error) {
	if shouldSkipKeyring() {
		// Test mode: load from file
		keyPath := signingKeyFilePath(configPath)
		data, err := os.ReadFile(keyPath)
		if err != nil {
			return nil, fmt.Errorf("signing key file not found (test mode): %w", err)
		}

		seed, err := base64.StdEncoding.DecodeString(strings.TrimSpace(string(data)))
		if err != nil {
			return nil, fmt.Errorf("invalid signing key encoding in file: %w", err)
		}

		if len(seed) != ed25519.SeedSize {
			return nil, fmt.Errorf("invalid signing key length in file: got %d, want %d", len(seed), ed25519.SeedSize)
		}

		return ed25519.NewKeyFromSeed(seed), nil
	}

	// Production: use OS keyring
	entryName := fmt.Sprintf("%s-%s", fingerprint, KeyringSigningSuffix)
	secretB64, err := keyring.Get(KeyringService, entryName)
	if err != nil {
		return nil, fmt.Errorf("signing key not found in keyring: %w", err)
	}

	seed, err := base64.StdEncoding.DecodeString(secretB64)
	if err != nil {
		return nil, fmt.Errorf("invalid signing key encoding in keyring: %w", err)
	}

	if len(seed) != ed25519.SeedSize {
		return nil, fmt.Errorf("invalid signing key length in keyring: got %d, want %d", len(seed), ed25519.SeedSize)
	}

	return ed25519.NewKeyFromSeed(seed), nil
}

// loadFromFile loads device from config file.
func loadFromFile(path string) (*Device, error) {
	data, err := os.ReadFile(path)
	if err != nil {
		return nil, fmt.Errorf("failed to read device config: %w", err)
	}

	var stored StoredDevice
	if err := json.Unmarshal(data, &stored); err != nil {
		return nil, fmt.Errorf("failed to parse device config: %w", err)
	}

	signingKey, err := loadSigningKey(path, stored.Fingerprint)
	if err != nil {
		return nil, fmt.Errorf("signing key not found. Device may need to be recreated: %w", err)
	}

	verifyingKey := signingKey.Public().(ed25519.PublicKey)

	return &Device{
		SigningKey:   signingKey,
		VerifyingKey: verifyingKey,
		Fingerprint:  stored.Fingerprint,
		Name:         stored.Name,
		DeviceID:     stored.DeviceID,
		configPath:   path,
	}, nil
}

// createNew creates a new device with fresh keypair.
func createNew(path string) (*Device, error) {
	// Generate Ed25519 keypair
	publicKey, privateKey, err := ed25519.GenerateKey(rand.Reader)
	if err != nil {
		return nil, fmt.Errorf("failed to generate keypair: %w", err)
	}

	// Compute fingerprint from public key
	fingerprint := ComputeFingerprint(publicKey)
	name := defaultName()

	// Store signing key (in keyring or file depending on environment)
	if err := storeSigningKey(path, fingerprint, privateKey); err != nil {
		return nil, err
	}

	// Store only public info in file
	stored := StoredDevice{
		VerifyingKey: base64.StdEncoding.EncodeToString(publicKey),
		Fingerprint:  fingerprint,
		Name:         name,
	}

	content, err := json.MarshalIndent(stored, "", "  ")
	if err != nil {
		return nil, fmt.Errorf("failed to serialize device config: %w", err)
	}

	if err := os.WriteFile(path, content, 0600); err != nil {
		return nil, fmt.Errorf("failed to write device config: %w", err)
	}

	return &Device{
		SigningKey:   privateKey,
		VerifyingKey: publicKey,
		Fingerprint:  fingerprint,
		Name:         name,
		configPath:   path,
	}, nil
}

// ComputeFingerprint computes fingerprint from public key.
// The fingerprint is first 8 bytes of SHA256(public_key) as hex with colons.
func ComputeFingerprint(publicKey ed25519.PublicKey) string {
	hash := sha256.Sum256(publicKey)
	parts := make([]string, 8)
	for i := 0; i < 8; i++ {
		parts[i] = fmt.Sprintf("%02x", hash[i])
	}
	return strings.Join(parts, ":")
}

// defaultName generates default device name based on hostname.
func defaultName() string {
	hostname, err := os.Hostname()
	if err != nil || hostname == "" {
		return "Botster CLI"
	}
	return fmt.Sprintf("Botster CLI (%s)", hostname)
}

// VerifyingKeyBase64 returns the verifying key as base64 string.
func (d *Device) VerifyingKeyBase64() string {
	d.mu.RLock()
	defer d.mu.RUnlock()
	return base64.StdEncoding.EncodeToString(d.VerifyingKey)
}

// Save saves the device info to file.
func (d *Device) Save() error {
	d.mu.RLock()
	stored := StoredDevice{
		VerifyingKey: base64.StdEncoding.EncodeToString(d.VerifyingKey),
		Fingerprint:  d.Fingerprint,
		Name:         d.Name,
		DeviceID:     d.DeviceID,
	}
	path := d.configPath
	d.mu.RUnlock()

	content, err := json.MarshalIndent(stored, "", "  ")
	if err != nil {
		return fmt.Errorf("failed to serialize device config: %w", err)
	}

	if err := os.WriteFile(path, content, 0600); err != nil {
		return fmt.Errorf("failed to write device config: %w", err)
	}

	return nil
}

// SetDeviceID updates the device ID after server registration.
func (d *Device) SetDeviceID(id int64) error {
	d.mu.Lock()
	d.DeviceID = &id
	d.mu.Unlock()
	return d.Save()
}

// ClearDeviceID clears stale device ID (e.g., after database reset).
func (d *Device) ClearDeviceID() error {
	d.mu.Lock()
	if d.DeviceID == nil {
		d.mu.Unlock()
		return nil
	}
	d.DeviceID = nil
	d.mu.Unlock()
	return d.Save()
}

// GetDeviceID returns the current device ID.
func (d *Device) GetDeviceID() *int64 {
	d.mu.RLock()
	defer d.mu.RUnlock()
	return d.DeviceID
}

// GetFingerprint returns the device fingerprint.
func (d *Device) GetFingerprint() string {
	d.mu.RLock()
	defer d.mu.RUnlock()
	return d.Fingerprint
}

// GetName returns the device name.
func (d *Device) GetName() string {
	d.mu.RLock()
	defer d.mu.RUnlock()
	return d.Name
}

// Sign signs data using the device's signing key.
func (d *Device) Sign(data []byte) []byte {
	d.mu.RLock()
	defer d.mu.RUnlock()
	return ed25519.Sign(d.SigningKey, data)
}

// Verify verifies a signature using the device's public key.
func (d *Device) Verify(data, signature []byte) bool {
	d.mu.RLock()
	defer d.mu.RUnlock()
	return ed25519.Verify(d.VerifyingKey, data, signature)
}
