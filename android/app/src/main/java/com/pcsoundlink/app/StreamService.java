package com.pcsoundlink.app;

import android.app.Notification;
import android.app.NotificationChannel;
import android.app.NotificationManager;
import android.app.PendingIntent;
import android.app.Service;
import android.content.Context;
import android.content.Intent;
import android.net.wifi.WifiManager;
import android.os.Build;
import android.os.IBinder;
import android.os.PowerManager;

import androidx.annotation.Nullable;
import androidx.core.app.NotificationCompat;
import androidx.core.content.ContextCompat;

import java.util.List;

/**
 * Keeps the stream alive while the screen is off by running as a foreground
 * service and holding a Wi-Fi + partial wake lock.
 */
public class StreamService extends Service implements AudioEngine.Listener {
    private static final String CHANNEL_ID = "pcsound-stream";
    private static final int NOTIFICATION_ID = 42;
    private static final String ACTION_STOP = "com.pcsoundlink.app.STOP";

    private AudioEngine engine;
    private PowerManager.WakeLock wakeLock;
    private WifiManager.WifiLock wifiLock;
    private boolean foregroundStarted = false;

    public static void start(Context context) {
        Intent intent = new Intent(context, StreamService.class);
        ContextCompat.startForegroundService(context, intent);
    }

    public static void stop(Context context) {
        context.stopService(new Intent(context, StreamService.class));
    }

    @Override
    public void onCreate() {
        super.onCreate();
        engine = AudioEngine.get(this);
        engine.addListener(this);
        createChannel();
        startForegroundInternal(buildNotification(getString(R.string.notif_status_preparing)));
        acquireLocks();
    }

    @Override
    public int onStartCommand(@Nullable Intent intent, int flags, int startId) {
        if (intent != null && ACTION_STOP.equals(intent.getAction())) {
            engine.disconnect();
            stopSelf();
            return START_NOT_STICKY;
        }
        return START_NOT_STICKY;
    }

    @Nullable
    @Override
    public IBinder onBind(Intent intent) {
        return null;
    }

    @Override
    public void onDestroy() {
        engine.removeListener(this);
        engine.disconnect();
        releaseLocks();
        super.onDestroy();
    }

    // ---------------------------------------------------------------- notification

    private void createChannel() {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            NotificationManager manager = getSystemService(NotificationManager.class);
            if (manager != null && manager.getNotificationChannel(CHANNEL_ID) == null) {
                NotificationChannel channel = new NotificationChannel(CHANNEL_ID,
                        getString(R.string.notif_channel_name), NotificationManager.IMPORTANCE_LOW);
                channel.setDescription(getString(R.string.notif_channel_desc));
                channel.setShowBadge(false);
                channel.setSound(null, null);
                manager.createNotificationChannel(channel);
            }
        }
    }

    private Notification buildNotification(String text) {
        Intent open = new Intent(this, MainActivity.class);
        open.setFlags(Intent.FLAG_ACTIVITY_SINGLE_TOP);
        int flags = PendingIntent.FLAG_UPDATE_CURRENT;
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.M) {
            flags |= PendingIntent.FLAG_IMMUTABLE;
        }
        PendingIntent contentIntent = PendingIntent.getActivity(this, 0, open, flags);

        Intent stop = new Intent(this, StreamService.class).setAction(ACTION_STOP);
        PendingIntent stopIntent = PendingIntent.getService(this, 1, stop, flags);

        return new NotificationCompat.Builder(this, CHANNEL_ID)
                .setSmallIcon(R.drawable.ic_stat_audio)
                .setContentTitle(getString(R.string.app_name))
                .setContentText(text)
                .setContentIntent(contentIntent)
                .addAction(0, getString(R.string.btn_disconnect), stopIntent)
                .setOngoing(true)
                .setOnlyAlertOnce(true)
                .setCategory(NotificationCompat.CATEGORY_SERVICE)
                .setPriority(NotificationCompat.PRIORITY_LOW)
                .build();
    }

    private void startForegroundInternal(Notification notification) {
        if (!foregroundStarted) {
            startForeground(NOTIFICATION_ID, notification);
            foregroundStarted = true;
        } else {
            NotificationManager manager = getSystemService(NotificationManager.class);
            if (manager != null) {
                manager.notify(NOTIFICATION_ID, notification);
            }
        }
    }

    private void updateNotification(String text) {
        startForegroundInternal(buildNotification(text));
    }

    // ---------------------------------------------------------------- locks

    private void acquireLocks() {
        if (wakeLock == null) {
            PowerManager power = (PowerManager) getSystemService(POWER_SERVICE);
            if (power != null) {
                wakeLock = power.newWakeLock(PowerManager.PARTIAL_WAKE_LOCK, "pcsound:stream");
                wakeLock.setReferenceCounted(false);
            }
        }
        if (wifiLock == null) {
            WifiManager wifi = (WifiManager) getApplicationContext()
                    .getSystemService(Context.WIFI_SERVICE);
            if (wifi != null) {
                wifiLock = wifi.createWifiLock(WifiManager.WIFI_MODE_FULL_HIGH_PERF, "pcsound:stream");
                wifiLock.setReferenceCounted(false);
            }
        }
        if (wakeLock != null && !wakeLock.isHeld()) {
            wakeLock.acquire();
        }
        if (wifiLock != null && !wifiLock.isHeld()) {
            wifiLock.acquire();
        }
    }

    private void releaseLocks() {
        if (wakeLock != null && wakeLock.isHeld()) {
            wakeLock.release();
        }
        if (wifiLock != null && wifiLock.isHeld()) {
            wifiLock.release();
        }
    }

    // ---------------------------------------------------------------- engine callbacks

    @Override
    public void onState(AudioEngine.State state, String message) {
        switch (state) {
            case SEARCHING:
                updateNotification(getString(R.string.status_title_searching));
                releaseLocks();
                break;
            case CONNECTING:
                updateNotification(getString(R.string.status_title_connecting));
                acquireLocks();
                break;
            case STREAMING:
                updateNotification(getString(R.string.status_title_streaming));
                acquireLocks();
                break;
            case ERROR:
                updateNotification(message == null || message.isEmpty()
                        ? getString(R.string.status_title_error) : message);
                releaseLocks();
                break;
            case IDLE:
            default:
                updateNotification(getString(R.string.notif_status_stopped));
                releaseLocks();
                break;
        }
    }

    @Override
    public void onServers(List<ServerInfo> servers) {
    }

    @Override
    public void onStats(AudioEngine.Stats stats) {
    }

    @Override
    public void onMute(boolean muted, boolean supported) {
    }

    @Override
    public void onLog(String line) {
    }
}
