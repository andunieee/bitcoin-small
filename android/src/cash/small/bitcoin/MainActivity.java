package cash.small.bitcoin;

import android.app.NativeActivity;
import android.content.Intent;
import android.os.Bundle;

/**
 * the activity, which only adds hearing what notification was tapped: its intent carries the
 * trade it is about. the rust side registers {@link #nativeOpen} and calls {@link #ready}, see
 * src/notify.rs.
 */
public class MainActivity extends NativeActivity {
    static final String TRADE = "trade";

    private static String pending;
    private static boolean ready;

    /** called by the rust side once it listens: the trade the app was started for, if any */
    public static synchronized String ready() {
        ready = true;
        String trade = pending;
        pending = null;
        return trade;
    }

    private static native void nativeOpen(String trade);

    private static void opened(Intent intent) {
        String trade = intent == null ? null : intent.getStringExtra(TRADE);
        if (trade == null) {
            return;
        }
        synchronized (MainActivity.class) {
            if (!ready) {
                pending = trade;
                return;
            }
        }
        nativeOpen(trade);
    }

    @Override
    protected void onCreate(Bundle state) {
        super.onCreate(state);
        opened(getIntent());
    }

    @Override
    protected void onNewIntent(Intent intent) {
        super.onNewIntent(intent);
        setIntent(intent);
        opened(intent);
    }
}
