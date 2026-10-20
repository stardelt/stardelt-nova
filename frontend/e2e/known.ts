// Findings that exist today and have a ticket. A finding that matches an entry is reported
// but does not fail the run; an entry that matches nothing fails it ("remove this line"), so
// the ticket that fixes the problem deletes its own entry and the list only ever shrinks.
// Never add an entry for a new finding: fix the page instead.
export type Known = { rule: string; route?: string; contains?: string; ticket: string }

export const KNOWN: Known[] = []
