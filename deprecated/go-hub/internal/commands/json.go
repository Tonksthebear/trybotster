// Package commands provides CLI subcommand implementations for botster-hub.
//
// This package contains utilities for JSON file manipulation, worktree
// management, and prompt retrieval.
package commands

import (
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"strings"
)

// JSONGet reads a value from a JSON file using dot-notation path.
//
// Navigates through the JSON structure using the provided key path and returns
// the resulting value as pretty-printed JSON.
func JSONGet(filePath, keyPath string) (string, error) {
	filePath = expandTilde(filePath)

	data, err := os.ReadFile(filePath)
	if err != nil {
		return "", fmt.Errorf("failed to read %s: %w", filePath, err)
	}

	var root interface{}
	if err := json.Unmarshal(data, &root); err != nil {
		return "", fmt.Errorf("failed to parse %s as JSON: %w", filePath, err)
	}

	// Navigate through the key path
	value := root
	for _, key := range strings.Split(keyPath, ".") {
		if key == "" {
			continue
		}
		obj, ok := value.(map[string]interface{})
		if !ok {
			return "", fmt.Errorf("key '%s' not found in path '%s'", key, keyPath)
		}
		value, ok = obj[key]
		if !ok {
			return "", fmt.Errorf("key '%s' not found in path '%s'", key, keyPath)
		}
	}

	result, err := json.MarshalIndent(value, "", "  ")
	if err != nil {
		return "", fmt.Errorf("failed to serialize value: %w", err)
	}

	return string(result), nil
}

// JSONSet sets a value in a JSON file using dot-notation path.
//
// Navigates to the specified location in the JSON structure and sets the value.
// Creates intermediate objects if they don't exist. The value is parsed as JSON
// first; if parsing fails, it's treated as a string.
func JSONSet(filePath, keyPath, newValue string) error {
	filePath = expandTilde(filePath)

	data, err := os.ReadFile(filePath)
	if err != nil {
		return fmt.Errorf("failed to read %s: %w", filePath, err)
	}

	var root map[string]interface{}
	if err := json.Unmarshal(data, &root); err != nil {
		return fmt.Errorf("failed to parse %s as JSON: %w", filePath, err)
	}

	// Parse the new value as JSON, fall back to string if parsing fails
	var parsedValue interface{}
	if err := json.Unmarshal([]byte(newValue), &parsedValue); err != nil {
		parsedValue = newValue
	}

	// Split the path and navigate/create structure
	keys := strings.Split(keyPath, ".")
	if len(keys) == 0 || (len(keys) == 1 && keys[0] == "") {
		return fmt.Errorf("empty key path")
	}

	// Navigate to the parent and set the final key
	current := root
	for i, key := range keys[:len(keys)-1] {
		if key == "" {
			continue
		}
		next, ok := current[key]
		if !ok {
			// Create intermediate object
			newObj := make(map[string]interface{})
			current[key] = newObj
			current = newObj
		} else {
			nextObj, ok := next.(map[string]interface{})
			if !ok {
				return fmt.Errorf("key '%s' at path index %d is not an object", key, i)
			}
			current = nextObj
		}
	}

	// Set the final value
	finalKey := keys[len(keys)-1]
	current[finalKey] = parsedValue

	// Write back to file
	result, err := json.MarshalIndent(root, "", "  ")
	if err != nil {
		return fmt.Errorf("failed to serialize JSON: %w", err)
	}

	if err := os.WriteFile(filePath, result, 0644); err != nil {
		return fmt.Errorf("failed to write %s: %w", filePath, err)
	}

	return nil
}

// JSONDelete deletes a key from a JSON file using dot-notation path.
func JSONDelete(filePath, keyPath string) error {
	filePath = expandTilde(filePath)

	data, err := os.ReadFile(filePath)
	if err != nil {
		return fmt.Errorf("failed to read %s: %w", filePath, err)
	}

	var root map[string]interface{}
	if err := json.Unmarshal(data, &root); err != nil {
		return fmt.Errorf("failed to parse %s as JSON: %w", filePath, err)
	}

	// Split the path
	keys := strings.Split(keyPath, ".")
	if len(keys) == 0 {
		return fmt.Errorf("empty key path")
	}

	// Navigate to the parent
	current := root
	for _, key := range keys[:len(keys)-1] {
		if key == "" {
			continue
		}
		next, ok := current[key]
		if !ok {
			return fmt.Errorf("key '%s' not found", key)
		}
		nextObj, ok := next.(map[string]interface{})
		if !ok {
			return fmt.Errorf("key '%s' is not an object", key)
		}
		current = nextObj
	}

	// Delete the final key
	finalKey := keys[len(keys)-1]
	if _, ok := current[finalKey]; !ok {
		return fmt.Errorf("key '%s' not found", finalKey)
	}
	delete(current, finalKey)

	// Write back to file
	result, err := json.MarshalIndent(root, "", "  ")
	if err != nil {
		return fmt.Errorf("failed to serialize JSON: %w", err)
	}

	if err := os.WriteFile(filePath, result, 0644); err != nil {
		return fmt.Errorf("failed to write %s: %w", filePath, err)
	}

	return nil
}

// expandTilde expands ~ to the user's home directory.
func expandTilde(path string) string {
	if strings.HasPrefix(path, "~/") {
		home, err := os.UserHomeDir()
		if err != nil {
			return path
		}
		return filepath.Join(home, path[2:])
	}
	return path
}
