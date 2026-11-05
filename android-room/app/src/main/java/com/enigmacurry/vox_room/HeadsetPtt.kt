package com.enigmacurry.vox_room

import android.bluetooth.BluetoothAssignedNumbers
import android.bluetooth.BluetoothHeadset
import android.bluetooth.BluetoothProfile
import android.content.Intent
import android.content.IntentFilter

/**
 * A headset that has a talk button of its own.
 *
 * The volume keys are a phone with no keyboard being lent two buttons
 * it already had -- see [TalkKey] -- and everything awkward about them
 * comes from their having another job. A radio-style speaker-mic has
 * the button push to talk was drawn for: one switch, momentary, whose
 * only purpose is to open a microphone. There is nothing to weigh up
 * and nothing to tell apart.
 *
 * **It does not arrive as a key.** A BTECH BS-22 pairs as A2DP and
 * hands-free and registers no HID device at all, so nothing about it
 * reaches `dispatchKeyEvent` or an input device node, and there is no
 * key code to bind. What the button sends is a vendor-specific
 * hands-free AT command, `AT+XEVENT=PTT_DOWN` and `AT+XEVENT=PTT_UP`,
 * which is Plantronics' extension and which Android forwards to
 * whoever asks as [BluetoothHeadset.ACTION_VENDOR_SPECIFIC_HEADSET_EVENT].
 * The platform's own `RemoteDevices` is already listening to that
 * broadcast to read battery levels out of it, and says so in the log
 * when it sees one of these and puts it down again:
 *
 *     getBatteryLevelFromXEventVsc() skip none BATTERY event: PTT_DOWN
 *
 * That line is how this was found, and it is also the proof the
 * broadcast is sent, since reading it is the only way that function is
 * ever called.
 *
 * **Down and up, really.** Measured over adb, on the button and not
 * from a datasheet: holds of 206 ms, 506 ms, 3033 ms, 3061 ms, 4813 ms
 * and 5099 ms each came through as one `PTT_DOWN` and one `PTT_UP` the
 * held distance apart, with no repeat in between and nothing latched
 * afterwards. So this is a hold and not a toggle, and the hold is the
 * one the hand made. A third event, `AT+XEVENT=BLE`, trails every
 * release by about 30 ms; it is not the button and is ignored by not
 * being named here.
 *
 * **Only what was measured is named.** Other makes of PTT headset spell
 * this differently, and a spelling nobody here has held in their hand
 * would be a guess written where it reads as a fact.
 */
object HeadsetPtt {
    /// The vendor extension the button speaks.
    ///
    /// Spelled out rather than named: the platform has a constant for
    /// this -- `BluetoothHeadset.VENDOR_SPECIFIC_HEADSET_EVENT_XEVENT`
    /// -- and keeps it hidden, while publishing the broadcast that
    /// carries it and the category that subscribes to it. So the one
    /// string an app needs in order to use a public API is the one it
    /// is not given, and writing it out is the whole of the workaround.
    private const val XEVENT = "+XEVENT"

    /// What the button says, in the words it says it in.
    private const val DOWN = "PTT_DOWN"
    private const val UP = "PTT_UP"

    /// The broadcast the button arrives as.
    ///
    /// The company id is part of the subscription and not of the
    /// message: Android puts a vendor event out under a category naming
    /// whose extension it is, so a filter without that category matches
    /// nothing at all. `+XEVENT` is Plantronics', whoever made the
    /// headset -- it is the extension being spoken that is theirs.
    fun buttonFilter(): IntentFilter =
        IntentFilter(BluetoothHeadset.ACTION_VENDOR_SPECIFIC_HEADSET_EVENT).apply {
            addCategory(
                BluetoothHeadset.VENDOR_SPECIFIC_HEADSET_EVENT_COMPANY_ID_CATEGORY +
                    "." + BluetoothAssignedNumbers.PLANTRONICS
            )
        }

    /// The headset going away, which is the one way a hold can end
    /// without a release. See [MainActivity.squeeze].
    ///
    /// A second registration rather than a second action on the one
    /// above: every category in a filter has to be matched, and this
    /// broadcast carries none, so a filter asking for Plantronics
    /// would never see it.
    fun goneFilter(): IntentFilter =
        IntentFilter(BluetoothHeadset.ACTION_CONNECTION_STATE_CHANGED)

    /// Whether the talk button went down (`true`), came up (`false`), or
    /// this was not the talk button at all (`null`).
    @Suppress("DEPRECATION")
    fun pressed(intent: Intent): Boolean? {
        if (intent.action != BluetoothHeadset.ACTION_VENDOR_SPECIFIC_HEADSET_EVENT) return null
        val cmd = intent.getStringExtra(BluetoothHeadset.EXTRA_VENDOR_SPECIFIC_HEADSET_EVENT_CMD)
        if (cmd != XEVENT) return null
        val args = intent.getSerializableExtra(
            BluetoothHeadset.EXTRA_VENDOR_SPECIFIC_HEADSET_EVENT_ARGS
        ) as? Array<*> ?: return null
        return when (args.firstOrNull() as? String) {
            DOWN -> true
            UP -> false
            else -> null
        }
    }

    /// Whether this says the headset is no longer connected.
    fun gone(intent: Intent): Boolean =
        intent.action == BluetoothHeadset.ACTION_CONNECTION_STATE_CHANGED &&
            intent.getIntExtra(BluetoothProfile.EXTRA_STATE, -1) ==
            BluetoothProfile.STATE_DISCONNECTED
}
