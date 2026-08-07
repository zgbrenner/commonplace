import { describe, expect, it } from "vitest";
import {
  formatCount,
  formatDateTime,
  formatDuration,
  formatFileSize,
  formatListTimestamp,
} from "./format";

// Every assertion names its locale and time zone. Nothing here may depend on
// the machine running the suite: a test that passes only in Europe/London is
// a test of the machine, not of the code.
const UTC = "UTC";
const AFTERNOON = "2026-08-07T14:30:00Z";

describe("formatDateTime", () => {
  it("writes the date in the reader's order", () => {
    expect(formatDateTime(AFTERNOON, "en-GB", UTC)).toBe("7 Aug 2026, 14:30");
    expect(formatDateTime(AFTERNOON, "de-DE", UTC)).toBe("07.08.2026, 14:30");
  });

  it("shows the same instant in the reader's zone", () => {
    expect(formatDateTime(AFTERNOON, "en-GB", "Australia/Sydney")).toBe("8 Aug 2026, 00:30");
  });

  it("returns nothing for a value that is not a date", () => {
    expect(formatDateTime("not a date", "en-GB", UTC)).toBe("");
    expect(formatDateTime("", "en-GB", UTC)).toBe("");
  });
});

describe("formatListTimestamp", () => {
  const now = new Date("2026-08-07T18:00:00Z");

  it("gives the time of day for something from today", () => {
    expect(formatListTimestamp(AFTERNOON, now, "en-GB", UTC)).toBe("14:30");
  });

  it("gives day and month within the same year", () => {
    expect(formatListTimestamp("2026-02-03T09:00:00Z", now, "en-GB", UTC)).toBe("3 Feb");
    expect(formatListTimestamp("2026-02-03T09:00:00Z", now, "de-DE", UTC)).toBe("3. Feb.");
  });

  it("gives the whole date once the year differs", () => {
    expect(formatListTimestamp("2025-12-31T23:00:00Z", now, "en-GB", UTC)).toBe("31 Dec 2025");
  });

  it("decides today by the reader's day boundary, not by UTC's", () => {
    // `now` is already the 8th in Sydney, so 09:00 UTC on the 7th — still
    // today in UTC — belongs to yesterday for a reader there.
    const morning = "2026-08-07T09:00:00Z";
    expect(formatListTimestamp(morning, now, "en-GB", UTC)).toBe("09:00");
    expect(formatListTimestamp(morning, now, "en-GB", "Australia/Sydney")).toBe("7 Aug");
  });

  it("returns nothing for a value that is not a date", () => {
    expect(formatListTimestamp("nonsense", now, "en-GB", UTC)).toBe("");
  });
});

describe("formatCount", () => {
  it("groups digits the way the reader groups them", () => {
    expect(formatCount(1200, "en-GB")).toBe("1,200");
    expect(formatCount(1200, "de-DE")).toBe("1.200");
  });

  it("leaves small numbers alone", () => {
    expect(formatCount(7, "de-DE")).toBe("7");
  });

  it("returns nothing for a number that is not one", () => {
    expect(formatCount(Number.NaN, "en-GB")).toBe("");
  });
});

describe("formatFileSize", () => {
  it("counts bytes below a kilobyte", () => {
    expect(formatFileSize(920, "en-GB")).toBe("920 byte");
  });

  it("steps up to the largest readable unit", () => {
    expect(formatFileSize(2_400_000, "en-GB")).toBe("2.4 MB");
    expect(formatFileSize(1_250_000_000, "en-GB")).toBe("1.3 GB");
  });

  it("uses the reader's decimal mark and unit symbol", () => {
    expect(formatFileSize(2_400_000, "de-DE")).toBe("2,4 MB");
    // French separates value from symbol with a narrow no-break space, which
    // is exactly the kind of detail hand-written formatting gets wrong.
    expect(formatFileSize(2_400_000, "fr-FR")).toBe("2,4\u202fMo");
  });

  it("returns nothing for a size that cannot exist", () => {
    expect(formatFileSize(-1, "en-GB")).toBe("");
    expect(formatFileSize(Number.NaN, "en-GB")).toBe("");
  });
});

describe("formatDuration", () => {
  it("does not pretend to sub-second precision", () => {
    expect(formatDuration(400, "en-GB")).toBe("less than a second");
  });

  it("counts seconds, then minutes and seconds, then hours and minutes", () => {
    expect(formatDuration(9_000, "en-GB")).toBe("9 seconds");
    expect(formatDuration(134_000, "en-GB")).toBe("2 minutes 14 seconds");
    expect(formatDuration(120_000, "en-GB")).toBe("2 minutes");
    expect(formatDuration(3_780_000, "en-GB")).toBe("1 hour 3 minutes");
    expect(formatDuration(7_200_000, "en-GB")).toBe("2 hours");
  });

  it("uses the singular for one", () => {
    expect(formatDuration(1_000, "en-GB")).toBe("1 second");
    expect(formatDuration(60_000, "en-GB")).toBe("1 minute");
  });

  it("carries a rounded-up second into the minutes", () => {
    expect(formatDuration(59_600, "en-GB")).toBe("1 minute");
    expect(formatDuration(119_600, "en-GB")).toBe("2 minutes");
  });

  it("keeps the unit words in the interface's language and the numeral in the reader's", () => {
    expect(formatDuration(74_000, "de-DE")).toBe("1 minute 14 seconds");
    expect(formatDuration(5_400_000, "de-DE")).toBe("1 hour 30 minutes");
  });

  it("returns nothing for a duration that cannot exist", () => {
    expect(formatDuration(-5, "en-GB")).toBe("");
  });
});
