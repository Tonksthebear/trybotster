// Package hub provides central state management for botster-hub.
//
// This file contains message-to-action conversion logic.
// Server messages are converted to HubActions for uniform processing.
package hub

import (
	"fmt"
	"path/filepath"
	"strings"

	"github.com/trybotster/botster-hub/internal/server"
)

// MessageContext provides context needed for message-to-action conversion.
type MessageContext struct {
	RepoPath          string
	RepoName          string
	WorktreeBase      string
	MaxSessions       int
	CurrentAgentCount int
}

// MessageError represents errors during message processing.
type MessageError struct {
	Kind    MessageErrorKind
	Field   string
	Message string
}

// MessageErrorKind identifies the type of message error.
type MessageErrorKind int

const (
	ErrMissingField MessageErrorKind = iota
	ErrMaxSessionsReached
)

func (e *MessageError) Error() string {
	switch e.Kind {
	case ErrMissingField:
		return fmt.Sprintf("missing required field: %s", e.Field)
	case ErrMaxSessionsReached:
		return fmt.Sprintf("maximum concurrent sessions (%s) reached", e.Message)
	default:
		return e.Message
	}
}

// MissingFieldError creates a missing field error.
func MissingFieldError(field string) *MessageError {
	return &MessageError{Kind: ErrMissingField, Field: field}
}

// MaxSessionsError creates a max sessions error.
func MaxSessionsError(max int) *MessageError {
	return &MessageError{Kind: ErrMaxSessionsReached, Message: fmt.Sprintf("%d", max)}
}

// MessageToHubAction converts a parsed server message to a Hub action.
// Returns nil action for messages that don't need Hub processing (e.g., WebRTC).
func MessageToHubAction(msg *server.ParsedMessage, ctx *MessageContext) (*HubAction, *MessageError) {
	// Handle cleanup messages
	if msg.IsCleanup() {
		if msg.IssueNumber == nil {
			return nil, MissingFieldError("issue_number")
		}
		if msg.Repo == "" {
			return nil, MissingFieldError("repo")
		}

		repoSafe := strings.ReplaceAll(msg.Repo, "/", "-")
		sessionKey := fmt.Sprintf("%s-%d", repoSafe, *msg.IssueNumber)

		action := CloseAgentAction(sessionKey, false)
		return &action, nil
	}

	// Handle WebRTC offers (not a Hub action - handled separately)
	if msg.IsWebRTCOffer() {
		return nil, nil
	}

	// Check max sessions limit
	if ctx.CurrentAgentCount >= ctx.MaxSessions {
		return nil, MaxSessionsError(ctx.MaxSessions)
	}

	// Spawn agent for this issue
	if msg.IssueNumber == nil {
		return nil, MissingFieldError("issue_number")
	}

	branchName := fmt.Sprintf("botster-issue-%d", *msg.IssueNumber)
	worktreePath := filepath.Join(ctx.WorktreeBase, branchName)

	msgID := msg.MessageID
	invURL := msg.InvocationURL

	action := SpawnAgentAction(
		msg.IssueNumber,
		branchName,
		worktreePath,
		ctx.RepoPath,
		ctx.RepoName,
		msg.TaskDescription(),
		&msgID,
		invURL,
	)

	return &action, nil
}
