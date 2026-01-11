// Package server provides server communication types.
//
// This file contains message parsing logic.
// Messages from the Rails server are parsed into ParsedMessage for easier processing.
package server

import (
	"fmt"
	"strings"
)

// ParsedMessage contains extracted fields from a server message.
type ParsedMessage struct {
	MessageID     int64
	EventType     string
	Repo          string
	IssueNumber   *int
	Prompt        string
	InvocationURL string
	CommentAuthor string
	CommentBody   string
}

// FromMessage parses a Message into a ParsedMessage.
func FromMessage(msg *Message) *ParsedMessage {
	issueNum := msg.IssueNumber()
	return &ParsedMessage{
		MessageID:     msg.ID,
		EventType:     msg.EventType,
		Repo:          msg.Repo(),
		IssueNumber:   issueNum,
		Prompt:        msg.Prompt(),
		InvocationURL: msg.InvocationURL(),
		CommentAuthor: msg.CommentAuthor(),
		CommentBody:   msg.CommentBody(),
	}
}

// IsCleanup returns true if this is a cleanup message.
func (p *ParsedMessage) IsCleanup() bool {
	return p.EventType == "agent_cleanup"
}

// IsWebRTCOffer returns true if this is a WebRTC offer message.
func (p *ParsedMessage) IsWebRTCOffer() bool {
	return p.EventType == "webrtc_offer"
}

// FormatNotification returns a notification string for pinging an existing agent.
func (p *ParsedMessage) FormatNotification() string {
	if p.Prompt != "" {
		return fmt.Sprintf("=== NEW MENTION (automated notification) ===\n\n%s\n\n==================", p.Prompt)
	}

	author := p.CommentAuthor
	if author == "" {
		author = "unknown"
	}
	body := p.CommentBody
	if body == "" {
		body = "New mention"
	}

	return fmt.Sprintf("=== NEW MENTION (automated notification) ===\n%s mentioned you: %s\n==================", author, body)
}

// TaskDescription returns the task description for spawning a new agent.
func (p *ParsedMessage) TaskDescription() string {
	if p.Prompt != "" {
		return p.Prompt
	}
	if p.CommentBody != "" {
		return p.CommentBody
	}
	return "Work on this issue"
}


// SessionKeyFromMessage generates a session key for an agent based on message data.
func SessionKeyFromMessage(repo string, issueNumber int) string {
	repoSafe := strings.ReplaceAll(repo, "/", "-")
	return fmt.Sprintf("%s-%d", repoSafe, issueNumber)
}
