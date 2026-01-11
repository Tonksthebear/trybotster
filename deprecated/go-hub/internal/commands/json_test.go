package commands

import (
	"encoding/json"
	"os"
	"path/filepath"
	"testing"
)

func TestJSONGet(t *testing.T) {
	dir := t.TempDir()
	filePath := filepath.Join(dir, "test.json")

	// Create test JSON file
	testData := map[string]interface{}{
		"root": map[string]interface{}{
			"nested": map[string]interface{}{
				"value": "hello",
				"number": 42,
			},
		},
		"simple": "world",
	}
	data, _ := json.MarshalIndent(testData, "", "  ")
	os.WriteFile(filePath, data, 0644)

	// Test getting nested value
	result, err := JSONGet(filePath, "root.nested.value")
	if err != nil {
		t.Fatalf("JSONGet failed: %v", err)
	}
	if result != `"hello"` {
		t.Errorf("got %s, want '\"hello\"'", result)
	}

	// Test getting number
	result, err = JSONGet(filePath, "root.nested.number")
	if err != nil {
		t.Fatalf("JSONGet failed: %v", err)
	}
	if result != "42" {
		t.Errorf("got %s, want '42'", result)
	}

	// Test getting simple value
	result, err = JSONGet(filePath, "simple")
	if err != nil {
		t.Fatalf("JSONGet failed: %v", err)
	}
	if result != `"world"` {
		t.Errorf("got %s, want '\"world\"'", result)
	}
}

func TestJSONGetNotFound(t *testing.T) {
	dir := t.TempDir()
	filePath := filepath.Join(dir, "test.json")

	testData := map[string]interface{}{"key": "value"}
	data, _ := json.MarshalIndent(testData, "", "  ")
	os.WriteFile(filePath, data, 0644)

	_, err := JSONGet(filePath, "nonexistent")
	if err == nil {
		t.Error("expected error for nonexistent key")
	}

	_, err = JSONGet(filePath, "key.nested")
	if err == nil {
		t.Error("expected error for accessing non-object")
	}
}

func TestJSONGetFileNotFound(t *testing.T) {
	_, err := JSONGet("/nonexistent/path/file.json", "key")
	if err == nil {
		t.Error("expected error for nonexistent file")
	}
}

func TestJSONGetInvalidJSON(t *testing.T) {
	dir := t.TempDir()
	filePath := filepath.Join(dir, "test.json")
	os.WriteFile(filePath, []byte("not json"), 0644)

	_, err := JSONGet(filePath, "key")
	if err == nil {
		t.Error("expected error for invalid JSON")
	}
}

func TestJSONSet(t *testing.T) {
	dir := t.TempDir()
	filePath := filepath.Join(dir, "test.json")

	// Create initial JSON
	testData := map[string]interface{}{"existing": "value"}
	data, _ := json.MarshalIndent(testData, "", "  ")
	os.WriteFile(filePath, data, 0644)

	// Set a new value
	if err := JSONSet(filePath, "new.nested.key", "test"); err != nil {
		t.Fatalf("JSONSet failed: %v", err)
	}

	// Verify
	result, err := JSONGet(filePath, "new.nested.key")
	if err != nil {
		t.Fatalf("JSONGet failed: %v", err)
	}
	if result != `"test"` {
		t.Errorf("got %s, want '\"test\"'", result)
	}

	// Verify existing value preserved
	result, err = JSONGet(filePath, "existing")
	if err != nil {
		t.Fatalf("JSONGet failed: %v", err)
	}
	if result != `"value"` {
		t.Errorf("got %s, want '\"value\"'", result)
	}
}

func TestJSONSetJSONValue(t *testing.T) {
	dir := t.TempDir()
	filePath := filepath.Join(dir, "test.json")

	// Create initial JSON
	testData := map[string]interface{}{}
	data, _ := json.MarshalIndent(testData, "", "  ")
	os.WriteFile(filePath, data, 0644)

	// Set a boolean value
	if err := JSONSet(filePath, "enabled", "true"); err != nil {
		t.Fatalf("JSONSet failed: %v", err)
	}

	result, err := JSONGet(filePath, "enabled")
	if err != nil {
		t.Fatalf("JSONGet failed: %v", err)
	}
	if result != "true" {
		t.Errorf("got %s, want 'true'", result)
	}

	// Set a number value
	if err := JSONSet(filePath, "count", "42"); err != nil {
		t.Fatalf("JSONSet failed: %v", err)
	}

	result, err = JSONGet(filePath, "count")
	if err != nil {
		t.Fatalf("JSONGet failed: %v", err)
	}
	if result != "42" {
		t.Errorf("got %s, want '42'", result)
	}
}

func TestJSONSetOverwriteExisting(t *testing.T) {
	dir := t.TempDir()
	filePath := filepath.Join(dir, "test.json")

	testData := map[string]interface{}{"key": "old"}
	data, _ := json.MarshalIndent(testData, "", "  ")
	os.WriteFile(filePath, data, 0644)

	if err := JSONSet(filePath, "key", "new"); err != nil {
		t.Fatalf("JSONSet failed: %v", err)
	}

	result, err := JSONGet(filePath, "key")
	if err != nil {
		t.Fatalf("JSONGet failed: %v", err)
	}
	if result != `"new"` {
		t.Errorf("got %s, want '\"new\"'", result)
	}
}

func TestJSONDelete(t *testing.T) {
	dir := t.TempDir()
	filePath := filepath.Join(dir, "test.json")

	testData := map[string]interface{}{
		"keep": "value",
		"delete": "me",
	}
	data, _ := json.MarshalIndent(testData, "", "  ")
	os.WriteFile(filePath, data, 0644)

	if err := JSONDelete(filePath, "delete"); err != nil {
		t.Fatalf("JSONDelete failed: %v", err)
	}

	// Verify deleted
	_, err := JSONGet(filePath, "delete")
	if err == nil {
		t.Error("expected error for deleted key")
	}

	// Verify other key preserved
	result, err := JSONGet(filePath, "keep")
	if err != nil {
		t.Fatalf("JSONGet failed: %v", err)
	}
	if result != `"value"` {
		t.Errorf("got %s, want '\"value\"'", result)
	}
}

func TestJSONDeleteNested(t *testing.T) {
	dir := t.TempDir()
	filePath := filepath.Join(dir, "test.json")

	testData := map[string]interface{}{
		"parent": map[string]interface{}{
			"keep":   "value",
			"delete": "me",
		},
	}
	data, _ := json.MarshalIndent(testData, "", "  ")
	os.WriteFile(filePath, data, 0644)

	if err := JSONDelete(filePath, "parent.delete"); err != nil {
		t.Fatalf("JSONDelete failed: %v", err)
	}

	// Verify deleted
	_, err := JSONGet(filePath, "parent.delete")
	if err == nil {
		t.Error("expected error for deleted key")
	}

	// Verify sibling preserved
	result, err := JSONGet(filePath, "parent.keep")
	if err != nil {
		t.Fatalf("JSONGet failed: %v", err)
	}
	if result != `"value"` {
		t.Errorf("got %s, want '\"value\"'", result)
	}
}

func TestJSONDeleteNotFound(t *testing.T) {
	dir := t.TempDir()
	filePath := filepath.Join(dir, "test.json")

	testData := map[string]interface{}{"key": "value"}
	data, _ := json.MarshalIndent(testData, "", "  ")
	os.WriteFile(filePath, data, 0644)

	err := JSONDelete(filePath, "nonexistent")
	if err == nil {
		t.Error("expected error for nonexistent key")
	}
}

func TestExpandTilde(t *testing.T) {
	home, err := os.UserHomeDir()
	if err != nil {
		t.Skip("could not get home directory")
	}

	tests := []struct {
		input    string
		expected string
	}{
		{"~/test", filepath.Join(home, "test")},
		{"/absolute/path", "/absolute/path"},
		{"relative/path", "relative/path"},
		{"~", "~"}, // Only ~/... is expanded
	}

	for _, tt := range tests {
		result := expandTilde(tt.input)
		if result != tt.expected {
			t.Errorf("expandTilde(%q) = %q, want %q", tt.input, result, tt.expected)
		}
	}
}

func TestJSONGetEmptyPath(t *testing.T) {
	dir := t.TempDir()
	filePath := filepath.Join(dir, "test.json")

	testData := map[string]interface{}{"key": "value"}
	data, _ := json.MarshalIndent(testData, "", "  ")
	os.WriteFile(filePath, data, 0644)

	// Empty path should return the root
	result, err := JSONGet(filePath, "")
	if err != nil {
		t.Fatalf("JSONGet with empty path failed: %v", err)
	}
	// Should contain the root object
	if result == "" {
		t.Error("expected non-empty result for root object")
	}
}

func TestJSONSetEmptyPath(t *testing.T) {
	dir := t.TempDir()
	filePath := filepath.Join(dir, "test.json")

	testData := map[string]interface{}{}
	data, _ := json.MarshalIndent(testData, "", "  ")
	os.WriteFile(filePath, data, 0644)

	err := JSONSet(filePath, "", "value")
	if err == nil {
		t.Error("expected error for empty path")
	}
}
