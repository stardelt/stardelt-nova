// Findings that exist today and have a ticket. A finding that matches an entry is reported
// but does not fail the run; an entry that matches nothing fails it ("remove this line"), so
// the ticket that fixes the problem deletes its own entry and the list only ever shrinks.
// Never add an entry for a new finding: fix the page instead.
export type Known = { rule: string; route?: string; contains?: string; ticket: string }

export const KNOWN: Known[] = [
  { rule: 'U3', route: '/catalog', contains: '0 <h1>', ticket: '#162' },
  { rule: 'U3', route: '/query', contains: '0 <h1>', ticket: '#162' },
  { rule: 'U11', ticket: '#163' },
  { rule: 'U8', contains: 'ApiError', ticket: '#163' },
  { rule: 'U8', contains: 'an /api/ path', ticket: '#163' },
  { rule: 'U12', ticket: '#164' },
  { rule: 'U8', contains: 'MVP', ticket: '#165' },
  { rule: 'U8', contains: 'phase N', ticket: '#165' },
]
