package cash.small.bitcoin;

import android.app.Notification;
import android.app.NotificationChannel;
import android.app.NotificationManager;
import android.app.PendingIntent;
import android.app.Service;
import android.content.Intent;
import android.content.pm.ServiceInfo;
import android.os.Build;
import android.os.IBinder;

/**
 * does nothing but show a notification, which is what keeps the process (and the relay
 * connections the rust side holds) from being frozen while the app is in the background. the rust
 * side starts it while there are open offers or ongoing trades, and stops it afterwards, see
 * src/notify.rs.
 */
public class ForegroundService extends Service {
    private static final String CHANNEL = "background";
    private static final int ID = 1;

    @Override
    public IBinder onBind(Intent intent) {
        return null;
    }

    @Override
    public int onStartCommand(Intent intent, int flags, int startId) {
        NotificationManager manager = getSystemService(NotificationManager.class);
        manager.createNotificationChannel(
                new NotificationChannel(CHANNEL, "background", NotificationManager.IMPORTANCE_LOW));

        Notification.Builder builder =
                new Notification.Builder(this, CHANNEL)
                        .setSmallIcon(android.R.drawable.stat_notify_chat)
                        .setContentTitle("bitcoin smol")
                        .setContentText("listening for replies to your offers and trades")
                        .setOngoing(true);
        Intent launch = getPackageManager().getLaunchIntentForPackage(getPackageName());
        if (launch != null) {
            builder.setContentIntent(
                    PendingIntent.getActivity(this, 0, launch, PendingIntent.FLAG_IMMUTABLE));
        }

        if (Build.VERSION.SDK_INT >= 34) {
            startForeground(ID, builder.build(), ServiceInfo.FOREGROUND_SERVICE_TYPE_SPECIAL_USE);
        } else {
            startForeground(ID, builder.build());
        }
        // if the process dies there is nothing left to keep alive, so don't come back on our own
        return START_NOT_STICKY;
    }
}
