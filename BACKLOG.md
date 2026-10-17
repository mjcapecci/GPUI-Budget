# Backlog

Upcoming work for Budget, in priority order. Update an item's status as it
moves, and move it to **Done** with the date and PR when it ships.

**Statuses:** `To do` · `In progress` · `Done`

| ID | Title | Status | Priority |
| --- | --- | --- | --- |
| [BUD-2](#bud-2-search-and-filter-transactions) | Search and filter transactions | In progress | High |
| [BUD-3](#bud-3-recurring-transactions) | Recurring transactions | To do | Medium |
| [BUD-4](#bud-4-csv-export-and-backuprestore) | CSV export and backup/restore | To do | Medium |
| [BUD-5](#bud-5-keyboard-shortcuts) | Keyboard shortcuts | To do | Medium |
| [BUD-6](#bud-6-rename-categories-and-payment-methods) | Rename categories and payment methods | To do | Low |
| [BUD-7](#bud-7-month-over-month-trends) | Month-over-month trends | To do | Low |
| [BUD-8](#bud-8-jump-to-current-month) | Jump to current month | To do | Low |
| [BUD-9](#bud-9-totals-for-the-shown-transactions) | Totals for the shown transactions | To do | Low |
| [BUD-10](#bud-10-visual-refresh) | Visual refresh | To do | Low |

---

## BUD-2: Search and filter transactions

**Status:** In progress · **Priority:** High

**Problem:** The list only ever shows one month and can be sorted but not
filtered. Finding "when did I last pay the plumber?" means paging back month
by month. Budgets and Summary show which categories are over budget but you
can't click through to their transactions.

**Acceptance criteria:**
- [x] A search box above the list matches note, category and payment method.
- [x] A "This month / All time" toggle; all-time results show their month.
- [x] A category filter.
- [x] Clicking a budget row or a summary bar opens Transactions filtered to
      that category and month.

---

## BUD-3: Recurring transactions

**Status:** To do · **Priority:** Medium

**Problem:** Rent, salary, subscriptions and utilities have to be re-entered
every month. Budgets carry forward; transactions don't.

**Acceptance criteria:**
- [ ] A "Repeat monthly" option when adding or editing a transaction.
- [ ] Opening a month offers to add the recurring transactions due in it
      ("3 recurring transactions due · Add all"), never adding them twice.
- [ ] Recurring transactions can be stopped and edited.

**Notes:** A cheaper first step is a "Duplicate" or "Copy to this month" row
action.

---

## BUD-4: CSV export and backup/restore

**Status:** To do · **Priority:** Medium

**Problem:** All data lives in one SQLite file, backed up only automatically
before a schema migration. There's no way to get data into a spreadsheet or
to snapshot before a large import.

**Acceptance criteria:**
- [ ] Settings › Data: "Export transactions (CSV)" for this month or all time.
- [ ] "Back up database…" (can reuse `VACUUM INTO`) and "Restore from backup…".
- [ ] Shows the database path with "Reveal in Finder".

---

## BUD-5: Keyboard shortcuts

**Status:** To do · **Priority:** Medium

**Problem:** The only shortcut is ⌘V on Import. Moving between months,
starting a new entry, cancelling an edit and switching pages all need the
mouse.

**Acceptance criteria:**
- [ ] ⌘[ / ⌘] previous and next month.
- [ ] ⌘N focuses the Amount field for a new transaction.
- [ ] Esc cancels an edit.
- [ ] ⌘1–⌘4 switch sidebar pages.
- [ ] ⌘F focuses search (after BUD-2).
- [ ] Shortcuts are shown in tooltips.

---

## BUD-6: Rename categories and payment methods

**Status:** To do · **Priority:** Low

**Problem:** Fixing a typo means deleting and re-adding. Re-adding only
restores transactions on an exact name match, and the category's budget
history is lost.

**Acceptance criteria:**
- [ ] Rename in place from Settings, keeping transactions and budgets.
- [ ] Same validation as adding (unique, not "Unassigned").

---

## BUD-7: Month-over-month trends

**Status:** To do · **Priority:** Low

**Problem:** Summary only shows the selected month.

**Acceptance criteria:**
- [ ] A 6–12 month spending chart on Summary.
- [ ] "vs. last month" change on the Income, Expenses and Net cards.

---

## BUD-8: Jump to current month

**Status:** To do · **Priority:** Low

**Acceptance criteria:**
- [ ] A "Today" button in the month switcher, shown when browsing another
      month.

---

## BUD-9: Totals for the shown transactions

**Status:** To do · **Priority:** Low

**Acceptance criteria:**
- [ ] A footer under the transaction list with the count, income and
      expense totals of the transactions shown. Most useful after BUD-2.

---

## BUD-10: Visual refresh

**Status:** To do · **Priority:** Low

**Problem:** The UI works but is plain to look at: flat dark panels, long
undivided rows, and little visual hierarchy between the form, filters and
list.

**Acceptance criteria:**
- [ ] Decide on a direction first (spacing, type scale, color accents, how
      income/expense and over-budget read at a glance).
- [ ] Apply it consistently across Transactions, Import, Budgets, Summary and
      Settings, in light and dark mode.

---

## Done

| ID | Title | Completed | Branch / PR |
| --- | --- | --- | --- |
| BUD-1 | Undo transaction delete | 2026-10-17 | `undo-delete` |

### BUD-1: Undo transaction delete

**Problem:** The Delete button on a transaction row removed it immediately,
with no confirmation and no undo.

**Shipped:**
- [x] Deleting a transaction shows an Undo bar ("Deleted Groceries · $60.00 ·
      note") with Undo and Dismiss buttons.
- [x] Undo restores the transaction exactly: same id, date, amount, note,
      category, payment method, and any remembered deleted category or
      payment method (`Db::restore_transaction`).
- [x] ⌘Z on the Transactions page undoes; text fields keep their own ⌘Z.
- [x] The bar disappears after 8 seconds, or when dismissed.
- [x] Deleting another transaction replaces the pending undo.
- [x] The undo is dropped if categories or payment methods change in the
      meantime.
