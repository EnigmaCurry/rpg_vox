// The longest name anyone may give themselves at the door. The server
// decides (room::NAME_MAX); this only stops the field going past it.
export const NAME_MAX = 24;

// The longest message anyone may type into the room (room::TYPED_MAX).
// The server decides this one too, and sanitizes besides: the field cap
// is only so the limit is felt while typing rather than on sending.
export const TYPED_MAX = 2000;
