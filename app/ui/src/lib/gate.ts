/**
 * The gate in words and colour, shared by the rail and the Status page.
 * The colour comes from the core's `GateIndicator`, the same value the tray
 * icon shows, so the window and the tray always agree.
 */
import { GATE_LABEL } from './strings';
import type { GateIndicator, StatusSnapshot } from './types';

/** One sentence describing the gate. Every indicator colour has its own words. */
export function gateHeadline(status: StatusSnapshot | null): string {
  if (!status) return GATE_LABEL.connecting;
  if (status.paused) return GATE_LABEL.paused;
  if (status.session_active && !status.override_active) return `Gate closed: ${status.game ?? 'a game'} is running`;
  if (status.session_active) return `Override active while ${status.game ?? 'the game'} is running`;
  if (status.cooling_down) return 'Game closed: waiting for the cooldown period before models are loaded';
  if (status.gate === 'held' || status.gate === 'draining' || status.gate === 'busy') return GATE_LABEL[status.gate];
  return GATE_LABEL.open;
}

const GATE_COLOUR: Record<GateIndicator, string> = {
  paused: 'var(--paused)',
  held: 'var(--held)',
  draining: 'var(--drain)',
  closed: 'var(--closed)',
  busy: 'var(--busy)',
  open: 'var(--open)',
};

/** CSS colour variable for the gate state. */
export function gateColour(status: StatusSnapshot | null): string {
  return GATE_COLOUR[status?.gate ?? 'paused'];
}
