import { test } from "node:test";
import assert from "node:assert/strict";
import { elapsedPosition, parseSkipSeconds, acceptSettingsSnapshot } from "./playback.ts";

test("should_keep_pending_edit_if_old_snapshot_arrives", () => {
  // Arrange
  const pending = { speed: 1.5, skip_intro: 10, skip_outro: 20 };
  const old = { speed: 1, skip_intro: 0, skip_outro: 0 };
  // Act
  const accepted = acceptSettingsSnapshot(pending, old, false);
  // Assert
  assert.equal(accepted, false);
});

test("should_accept_settings_if_pending_snapshot_matches", () => {
  // Arrange
  const pending = { speed: 1.5, skip_intro: 10, skip_outro: 20 };
  // Act
  const accepted = acceptSettingsSnapshot(pending, { ...pending }, false);
  // Assert
  assert.equal(accepted, true);
});

test("should_accept_settings_if_folder_changes", () => {
  // Arrange
  const pending = { speed: 1.5, skip_intro: 10, skip_outro: 20 };
  // Act
  const accepted = acceptSettingsSnapshot(pending, { speed: 1, skip_intro: 0, skip_outro: 0 }, true);
  // Assert
  assert.equal(accepted, true);
});

for (const [speed, expected] of [[0.5, 10.5], [1, 11], [1.5, 11.5], [2, 12]]) {
  test(`should_estimate_original_position_if_playing_at_${speed}`, () => {
    // Arrange
    const position = 10;
    // Act
    const actual = elapsedPosition(position, 1000, true, speed);
    // Assert
    assert.equal(actual, expected);
  });
}

test("should_keep_position_if_paused", () => {
  // Arrange
  const position = 20;
  // Act
  const actual = elapsedPosition(position, 1000, false, 2);
  // Assert
  assert.equal(actual, position);
});

for (const value of ["", "-1", "1.5", "NaN", "Infinity", "4294967296"]) {
  test(`should_reject_seconds_if_input_is_${JSON.stringify(value)}`, () => {
    // Arrange
    const input = value;
    // Act
    const actual = parseSkipSeconds(input);
    // Assert
    assert.equal(actual, null);
  });
}

for (const value of ["0", "30", "4294967295"]) {
  test(`should_accept_seconds_if_input_is_${value}`, () => {
    // Arrange
    const input = value;
    // Act
    const actual = parseSkipSeconds(input);
    // Assert
    assert.equal(actual, Number(value));
  });
}
