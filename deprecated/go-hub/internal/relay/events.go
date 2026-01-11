package relay

import (
	"path/filepath"
	"strconv"

	"github.com/trybotster/botster-hub/internal/hub"
)

// BrowserEventContext provides context needed for event-to-action conversion.
type BrowserEventContext struct {
	WorktreeBase string
	RepoPath     string
	RepoName     string
}

// BrowserEventToHubAction converts a BrowserEvent to a HubAction.
// Returns nil for events that don't map to Hub actions (e.g., list/connection events).
func BrowserEventToHubAction(event *BrowserEvent, ctx *BrowserEventContext) *hub.HubAction {
	switch event.Type {
	case EventInput:
		action := hub.SendInputAction([]byte(event.Data))
		return &action

	case EventSelectAgent:
		action := hub.SelectByKeyAction(event.ID)
		return &action

	case EventCreateAgent:
		issueNumber, branchName := parseIssueOrBranch(event.IssueOrBranch)
		actualBranch := branchName
		if actualBranch == "" {
			if issueNumber != nil {
				actualBranch = "botster-issue-" + strconv.Itoa(*issueNumber)
			} else {
				actualBranch = "new-branch"
			}
		}

		worktreePath := filepath.Join("/tmp", actualBranch)
		if ctx.WorktreeBase != "" {
			worktreePath = filepath.Join(ctx.WorktreeBase, actualBranch)
		}

		prompt := ""
		if event.Prompt != nil {
			prompt = *event.Prompt
		}

		action := hub.SpawnAgentAction(
			issueNumber,
			actualBranch,
			worktreePath,
			ctx.RepoPath,
			ctx.RepoName,
			prompt,
			nil,
			"",
		)
		return &action

	case EventDeleteAgent:
		action := hub.CloseAgentAction(event.ID, event.DeleteWorktree)
		return &action

	case EventTogglePtyView:
		action := hub.TogglePTYViewAction()
		return &action

	case EventScroll:
		var action hub.HubAction
		switch event.Direction {
		case "up":
			action = hub.ScrollUpAction(int(event.Lines))
		case "down":
			action = hub.ScrollDownAction(int(event.Lines))
		default:
			return nil
		}
		return &action

	case EventScrollToBottom:
		action := hub.ScrollToBottomAction()
		return &action

	case EventScrollToTop:
		action := hub.ScrollToTopAction()
		return &action

	case EventResize:
		if event.Resize == nil {
			return nil
		}
		action := hub.ResizeAction(event.Resize.Rows, event.Resize.Cols)
		return &action

	// Events that don't map to Hub actions
	case EventConnected, EventDisconnected, EventListAgents, EventListWorktrees,
		EventReopenWorktree, EventSetMode:
		return nil

	default:
		return nil
	}
}

// parseIssueOrBranch parses a string into issue number and/or branch name.
func parseIssueOrBranch(value *string) (*int, string) {
	if value == nil || *value == "" {
		return nil, ""
	}

	// Try to parse as issue number
	if num, err := strconv.Atoi(*value); err == nil {
		return &num, ""
	}

	// Otherwise treat as branch name
	return nil, *value
}

// ResizeAction represents the result of checking browser resize state.
type ResizeAction int

const (
	ResizeNone ResizeAction = iota
	ResizeAgents
	ResetToLocal
)

// ResizeResult contains resize action details.
type ResizeResult struct {
	Action ResizeAction
	Rows   uint16
	Cols   uint16
}

// BrowserMode represents the browser display mode.
type BrowserMode int

const (
	BrowserModeTUI BrowserMode = iota
	BrowserModeGUI
)

// Resize state tracking (package-level for simplicity)
var (
	lastDims     uint32
	wasConnected bool
)

// CheckBrowserResize checks if browser dimensions have changed and returns resize action.
func CheckBrowserResize(browserDims *BrowserDimsWithMode, localDims [2]uint16) ResizeResult {
	isConnected := browserDims != nil
	prevConnected := wasConnected
	wasConnected = isConnected

	if browserDims != nil {
		rows := browserDims.Rows
		cols := browserDims.Cols
		mode := browserDims.Mode

		if cols >= 20 && rows >= 5 {
			modeBit := uint32(0)
			if mode == BrowserModeGUI {
				modeBit = 1 << 31
			}
			combined := modeBit | (uint32(cols) << 16) | uint32(rows)

			if lastDims != combined {
				lastDims = combined

				var agentCols, agentRows uint16
				if mode == BrowserModeGUI {
					agentCols = cols
					agentRows = rows
				} else {
					// TUI mode - use 70% width
					agentCols = (cols * 70 / 100) - 2
					agentRows = rows - 2
				}

				return ResizeResult{
					Action: ResizeAgents,
					Rows:   agentRows,
					Cols:   agentCols,
				}
			}
		}
		return ResizeResult{Action: ResizeNone}
	}

	if prevConnected {
		// Browser disconnected - reset to local terminal
		lastDims = 0
		localRows := localDims[0]
		localCols := localDims[1]
		termCols := (localCols * 70 / 100) - 2
		termRows := localRows - 2

		return ResizeResult{
			Action: ResetToLocal,
			Rows:   termRows,
			Cols:   termCols,
		}
	}

	return ResizeResult{Action: ResizeNone}
}

// BrowserDimsWithMode contains browser dimensions and display mode.
type BrowserDimsWithMode struct {
	Rows uint16
	Cols uint16
	Mode BrowserMode
}

// BrowserResponse represents what to send back to browser after processing an event.
type BrowserResponse int

const (
	ResponseNone BrowserResponse = iota
	ResponseSendAgentList
	ResponseSendWorktreeList
	ResponseSendAgentSelected
)

// BrowserEventResult contains the result of processing a browser event.
type BrowserEventResult struct {
	Action           *hub.HubAction
	Resize           *[2]uint16
	InvalidateScreen bool
	Response         BrowserResponse
	AgentID          string // For SendAgentSelected
}
