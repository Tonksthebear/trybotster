// Package notification handles terminal OSC escape sequence detection.
//
// This module parses OSC (Operating System Command) escape sequences from PTY
// output that terminals use for notifications. Agents can use these to signal
// events like task completion.
//
// Supported notification types:
// - OSC 9: Simple notification (ESC ] 9 ; message BEL)
// - OSC 777: Rich notification (ESC ] 777 ; notify ; title ; body BEL)
package notification

import "strings"

// Type identifies the kind of notification.
type Type string

const (
	// TypeOSC9 is a simple notification with message.
	TypeOSC9 Type = "osc9"

	// TypeOSC777 is a rich notification with title and body.
	TypeOSC777 Type = "osc777"
)

// Notification represents a detected terminal notification.
type Notification struct {
	// Type is the notification type (osc9 or osc777).
	Type Type

	// Message is the notification message (OSC 9).
	Message string

	// Title is the notification title (OSC 777).
	Title string

	// Body is the notification body (OSC 777).
	Body string
}

// AgentStatus represents the lifecycle state of an agent.
type AgentStatus string

const (
	// StatusInitializing means the agent is starting up.
	StatusInitializing AgentStatus = "initializing"

	// StatusRunning means the agent is actively running.
	StatusRunning AgentStatus = "running"

	// StatusFinished means the agent completed successfully.
	StatusFinished AgentStatus = "finished"

	// StatusFailed means the agent failed with an error.
	StatusFailed AgentStatus = "failed"

	// StatusKilled means the agent was manually terminated.
	StatusKilled AgentStatus = "killed"
)

// Detect parses terminal notifications from raw PTY output.
//
// Parses OSC escape sequences and returns any detected notifications.
// Supports both BEL (0x07) and ST (ESC \) terminators.
//
// OSC 9 messages that look like escape sequences (only digits and semicolons)
// are filtered out to avoid false positives.
func Detect(data []byte) []Notification {
	var notifications []Notification

	// Parse OSC sequences (ESC ] ... BEL or ESC ] ... ESC \)
	i := 0
	for i < len(data) {
		// Check for OSC sequence start: ESC ]
		if i+1 < len(data) && data[i] == 0x1b && data[i+1] == ']' {
			// Find the end of the OSC sequence (BEL or ST)
			oscStart := i + 2
			oscEnd := -1

			for j := oscStart; j < len(data); j++ {
				if data[j] == 0x07 {
					// Ends with BEL
					oscEnd = j
					break
				} else if j+1 < len(data) && data[j] == 0x1b && data[j+1] == '\\' {
					// Ends with ST (ESC \)
					oscEnd = j
					break
				}
			}

			if oscEnd != -1 {
				oscContent := data[oscStart:oscEnd]

				// Parse OSC 9: notification
				if len(oscContent) > 2 && oscContent[0] == '9' && oscContent[1] == ';' {
					message := string(oscContent[2:])
					// Only add if message is meaningful (not just numbers/semicolons)
					if !isEscapeSequence(message) && message != "" {
						notifications = append(notifications, Notification{
							Type:    TypeOSC9,
							Message: message,
						})
					}
				} else if len(oscContent) > 11 && string(oscContent[:11]) == "777;notify;" {
					// Parse OSC 777: notify;title;body
					content := string(oscContent[11:])
					parts := strings.SplitN(content, ";", 2)
					title := ""
					body := ""
					if len(parts) > 0 {
						title = parts[0]
					}
					if len(parts) > 1 {
						body = parts[1]
					}
					// Only add if there's meaningful content
					if title != "" || body != "" {
						notifications = append(notifications, Notification{
							Type:  TypeOSC777,
							Title: title,
							Body:  body,
						})
					}
				}

				// Skip past the OSC sequence
				i = oscEnd + 1
				continue
			}
		}

		i++
	}

	return notifications
}

// isEscapeSequence returns true if the message looks like an escape sequence
// (only contains digits and semicolons).
func isEscapeSequence(s string) bool {
	if s == "" {
		return false
	}
	for _, c := range s {
		if !isDigitOrSemicolon(c) {
			return false
		}
	}
	return true
}

func isDigitOrSemicolon(c rune) bool {
	return (c >= '0' && c <= '9') || c == ';'
}
