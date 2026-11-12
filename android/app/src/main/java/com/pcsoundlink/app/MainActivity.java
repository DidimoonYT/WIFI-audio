package com.pcsoundlink.app;

import android.Manifest;
import android.content.ClipData;
import android.content.ClipboardManager;
import android.content.Context;
import android.content.Intent;
import android.content.SharedPreferences;
import android.content.pm.PackageManager;
import android.net.ConnectivityManager;
import android.net.Network;
import android.net.NetworkCapabilities;
import android.os.Build;
import android.os.Bundle;
import android.os.Handler;
import android.os.Looper;
import android.os.SystemClock;
import android.view.View;
import android.view.WindowManager;
import android.widget.AdapterView;
import android.widget.ArrayAdapter;
import android.widget.EditText;
import android.widget.ProgressBar;
import android.widget.SeekBar;
import android.widget.Spinner;
import android.widget.TextView;
import android.widget.Toast;

import androidx.appcompat.app.AppCompatActivity;
import androidx.appcompat.app.AppCompatDelegate;
import androidx.core.content.ContextCompat;
import androidx.core.os.LocaleListCompat;

import com.google.android.material.button.MaterialButton;
import com.google.android.material.card.MaterialCardView;
import com.google.android.material.materialswitch.MaterialSwitch;

import java.util.ArrayList;
import java.util.List;
import java.util.Locale;
import java.util.regex.Pattern;

public class MainActivity extends AppCompatActivity implements AudioEngine.Listener {
    private static final Pattern IPV4 = Pattern.compile("^\\d{1,3}(\\.\\d{1,3}){3}$");
    private static final Pattern HOSTNAME = Pattern.compile("^[A-Za-z0-9][A-Za-z0-9.\\-]{0,62}$");

    private AudioEngine engine;
    private SharedPreferences prefs;

    private TextView textStatus;
    private TextView textStatusDetail;
    private TextView textStatusDot;
    private TextView textStats;
    private TextView textLog;
    private TextView textVolume;
    private TextView textLatency;
    private ProgressBar progressBar;
    private Spinner spinnerServers;
    private MaterialButton btnConnect;
    private MaterialButton btnTouchpad;
    private MaterialButton btnDonate;
    private MaterialButton btnCopyPcLink;
    private EditText editManualIp;
    private MaterialSwitch switchAutoConnect;
    private MaterialSwitch switchMute;
    private SeekBar seekVolume;
    private SeekBar seekLatency;
    private MaterialCardView cardNetworkWarning;
    private TextView textNetworkWarning;
    private ConnectivityManager connectivityManager;
    private ConnectivityManager.NetworkCallback networkCallback;
    private boolean networkCallbackRegistered = false;
    private boolean vpnWarningShown = false;

    private final List<ServerInfo> servers = new ArrayList<>();
    private final List<String> serverLabels = new ArrayList<>();
    private ArrayAdapter<String> serverAdapter;
    private int selectedIndex = -1;
    private String selectedServerKey;
    private String connectedServerName;
    private boolean autoConnectSuppressed = false;
    private boolean muteProgrammatic = false;
    private long lastAutoConnectAttempt = 0;
    private final Handler handler = new Handler(Looper.getMainLooper());

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        setContentView(R.layout.activity_main);

        engine = AudioEngine.get(this);
        prefs = getSharedPreferences("pcsound", MODE_PRIVATE);

        bindViews();
        setupServers();
        setupControls();
        setupLanguage();
        setupNetworkWarning();
        requestNotificationPermission();

        renderState(engine.getState(), engine.getStateMessage());
    }

    private void bindViews() {
        textStatus = findViewById(R.id.textStatus);
        textStatusDetail = findViewById(R.id.textStatusDetail);
        textStatusDot = findViewById(R.id.textStatusDot);
        textStats = findViewById(R.id.textStats);
        textLog = findViewById(R.id.textLog);
        textVolume = findViewById(R.id.textVolume);
        textLatency = findViewById(R.id.textLatency);
        progressBar = findViewById(R.id.progressBar);
        spinnerServers = findViewById(R.id.spinnerServers);
        btnConnect = findViewById(R.id.btnConnect);
        btnTouchpad = findViewById(R.id.btnTouchpad);
        btnDonate = findViewById(R.id.btnDonate);
        btnCopyPcLink = findViewById(R.id.btnCopyPcLink);
        editManualIp = findViewById(R.id.editManualIp);
        switchAutoConnect = findViewById(R.id.switchAutoConnect);
        switchMute = findViewById(R.id.switchMute);
        seekVolume = findViewById(R.id.seekVolume);
        seekLatency = findViewById(R.id.seekLatency);
        cardNetworkWarning = findViewById(R.id.cardNetworkWarning);
        textNetworkWarning = findViewById(R.id.textNetworkWarning);
    }

    private void setupServers() {
        serverAdapter = new ArrayAdapter<>(this, android.R.layout.simple_spinner_item,
                serverLabels);
        serverAdapter.setDropDownViewResource(android.R.layout.simple_spinner_dropdown_item);
        spinnerServers.setAdapter(serverAdapter);
        spinnerServers.setOnItemSelectedListener(new AdapterView.OnItemSelectedListener() {
            @Override
            public void onItemSelected(AdapterView<?> parent, View view, int position, long id) {
                if (position >= 0 && position < servers.size()) {
                    selectedIndex = position;
                    selectedServerKey = servers.get(position).key();
                } else {
                    selectedIndex = -1;
                }
            }

            @Override
            public void onNothingSelected(AdapterView<?> parent) {
                selectedIndex = -1;
            }
        });
        updateServerList(engine.getServers());
    }

    private void setupControls() {
        findViewById(R.id.btnSearch).setOnClickListener(v -> {
            autoConnectSuppressed = false;
            engine.startDiscovery();
        });

        btnConnect.setOnClickListener(v -> {
            AudioEngine.State state = engine.getState();
            if (state == AudioEngine.State.CONNECTING
                    || state == AudioEngine.State.STREAMING) {
                autoConnectSuppressed = true;
                engine.disconnect();
                StreamService.stop(this);
            } else {
                connectToSelected();
            }
        });

        btnTouchpad.setOnClickListener(v -> openTouchpad());
        btnDonate.setOnClickListener(v -> openDonate());
        btnCopyPcLink.setOnClickListener(v -> copyPcLink());

        switchAutoConnect.setChecked(prefs.getBoolean("auto_connect", true));
        switchAutoConnect.setOnCheckedChangeListener((buttonView, isChecked) -> {
            prefs.edit().putBoolean("auto_connect", isChecked).apply();
            if (isChecked) {
                autoConnectSuppressed = false;
                maybeAutoConnect();
            }
        });

        switchMute.setOnCheckedChangeListener((buttonView, isChecked) -> {
            if (!muteProgrammatic) {
                engine.setMuted(isChecked);
            }
        });

        int volume = engine.getVolume();
        seekVolume.setProgress(volume);
        textVolume.setText(getString(R.string.volume_value, volume));
        seekVolume.setOnSeekBarChangeListener(new SimpleSeekListener() {
            @Override
            public void onProgressChanged(SeekBar seekBar, int progress, boolean fromUser) {
                engine.setVolume(progress);
                textVolume.setText(getString(R.string.volume_value, progress));
            }
        });

        int latency = engine.getLatency();
        seekLatency.setMax(18); // 20..200 ms in 10 ms steps
        seekLatency.setProgress((latency - 20) / 10);
        textLatency.setText(getString(R.string.latency_value, latency));
        seekLatency.setOnSeekBarChangeListener(new SimpleSeekListener() {
            @Override
            public void onProgressChanged(SeekBar seekBar, int progress, boolean fromUser) {
                int value = 20 + progress * 10;
                engine.setLatency(value);
                textLatency.setText(getString(R.string.latency_value, value));
            }
        });
    }

    private void setupLanguage() {
        Spinner spinner = findViewById(R.id.spinnerLanguage);
        String[] labels = {
                getString(R.string.language_system),
                getString(R.string.language_russian),
                getString(R.string.language_english)
        };
        ArrayAdapter<String> adapter = new ArrayAdapter<>(this,
                android.R.layout.simple_spinner_item, labels);
        adapter.setDropDownViewResource(android.R.layout.simple_spinner_dropdown_item);
        spinner.setAdapter(adapter);

        final String current = AppCompatDelegate.getApplicationLocales().toLanguageTags();
        int index = 0;
        if (current.startsWith("ru")) {
            index = 1;
        } else if (current.startsWith("en")) {
            index = 2;
        }
        spinner.setSelection(index);

        spinner.setOnItemSelectedListener(new AdapterView.OnItemSelectedListener() {
            @Override
            public void onItemSelected(AdapterView<?> parent, View view, int position, long id) {
                String tag = position == 1 ? "ru" : position == 2 ? "en" : "";
                if (tag.equals(current)) {
                    return;
                }
                AppCompatDelegate.setApplicationLocales(tag.isEmpty()
                        ? LocaleListCompat.getEmptyLocaleList()
                        : LocaleListCompat.forLanguageTags(tag));
            }

            @Override
            public void onNothingSelected(AdapterView<?> parent) {
            }
        });
    }

    private void requestNotificationPermission() {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU
                && ContextCompat.checkSelfPermission(this, Manifest.permission.POST_NOTIFICATIONS)
                != PackageManager.PERMISSION_GRANTED) {
            requestPermissions(new String[]{Manifest.permission.POST_NOTIFICATIONS}, 10);
        }
    }

    private void setupNetworkWarning() {
        connectivityManager =
                (ConnectivityManager) getSystemService(Context.CONNECTIVITY_SERVICE);
        networkCallback = new ConnectivityManager.NetworkCallback() {
            @Override
            public void onAvailable(Network network) {
                handler.post(MainActivity.this::updateNetworkWarning);
            }

            @Override
            public void onLost(Network network) {
                handler.post(MainActivity.this::updateNetworkWarning);
            }

            @Override
            public void onCapabilitiesChanged(Network network, NetworkCapabilities caps) {
                handler.post(MainActivity.this::updateNetworkWarning);
            }
        };
        updateNetworkWarning();
    }

    /** Показывает предупреждение, если включён VPN или телефон не в Wi-Fi. */
    private void updateNetworkWarning() {
        if (connectivityManager == null || cardNetworkWarning == null) {
            return;
        }
        boolean vpn = false;
        boolean mobileOnly = false;
        try {
            Network active = connectivityManager.getActiveNetwork();
            NetworkCapabilities caps = active == null
                    ? null : connectivityManager.getNetworkCapabilities(active);
            if (caps != null) {
                vpn = caps.hasTransport(NetworkCapabilities.TRANSPORT_VPN);
            }
            if (!vpn) {
                for (Network network : connectivityManager.getAllNetworks()) {
                    NetworkCapabilities other = connectivityManager.getNetworkCapabilities(network);
                    if (other != null && other.hasTransport(NetworkCapabilities.TRANSPORT_VPN)) {
                        vpn = true;
                        break;
                    }
                }
            }
            if (!vpn && caps != null) {
                boolean local = caps.hasTransport(NetworkCapabilities.TRANSPORT_WIFI)
                        || caps.hasTransport(NetworkCapabilities.TRANSPORT_ETHERNET);
                mobileOnly = caps.hasTransport(NetworkCapabilities.TRANSPORT_CELLULAR) && !local;
            }
        } catch (Exception ignored) {
            // Без доступа к сведениям о сетях просто не показываем предупреждение.
        }

        String message = null;
        if (vpn) {
            message = getString(R.string.network_warning_vpn);
        } else if (mobileOnly) {
            message = getString(R.string.network_warning_mobile);
        }
        if (message == null) {
            cardNetworkWarning.setVisibility(View.GONE);
        } else {
            textNetworkWarning.setText(message);
            cardNetworkWarning.setVisibility(View.VISIBLE);
        }

        if (vpn && !vpnWarningShown) {
            vpnWarningShown = true;
            Toast.makeText(this, R.string.toast_vpn_warning, Toast.LENGTH_LONG).show();
        } else if (!vpn) {
            vpnWarningShown = false;
        }
    }

    private void registerNetworkCallback() {
        if (connectivityManager == null || networkCallbackRegistered) {
            return;
        }
        try {
            connectivityManager.registerDefaultNetworkCallback(networkCallback);
            networkCallbackRegistered = true;
        } catch (Exception ignored) {
            // Не критично: предупреждение просто не будет обновляться.
        }
        updateNetworkWarning();
    }

    private void unregisterNetworkCallback() {
        if (connectivityManager == null || !networkCallbackRegistered) {
            return;
        }
        try {
            connectivityManager.unregisterNetworkCallback(networkCallback);
        } catch (Exception ignored) {
            // Сеть могла исчезнуть раньше — это нормально.
        }
        networkCallbackRegistered = false;
    }

    @Override
    public void onRequestPermissionsResult(int requestCode, String[] permissions,
                                           int[] grantResults) {
        super.onRequestPermissionsResult(requestCode, permissions, grantResults);
        if (requestCode == 10 && grantResults.length > 0
                && grantResults[0] != PackageManager.PERMISSION_GRANTED) {
            Toast.makeText(this, R.string.toast_need_notifications, Toast.LENGTH_LONG).show();
        }
    }

    // ----------------------------------------------------------------- lifecycle

    @Override
    protected void onStart() {
        super.onStart();
        engine.addListener(this);
        engine.startDiscovery();
        registerNetworkCallback();
    }

    @Override
    protected void onStop() {
        unregisterNetworkCallback();
        engine.removeListener(this);
        engine.stopDiscovery();
        super.onStop();
    }

    // ----------------------------------------------------------------- actions

    private void connectToSelected() {
        String manual = editManualIp.getText().toString().trim();
        ServerInfo target = null;
        if (!manual.isEmpty()) {
            String host = extractHost(manual);
            if (!isValidHost(host)) {
                Toast.makeText(this, R.string.toast_bad_ip, Toast.LENGTH_SHORT).show();
                return;
            }
            target = new ServerInfo("manual:" + host, getString(R.string.server_name_prefix, host),
                    host, Protocol.CONTROL_PORT, SystemClock.elapsedRealtime());
        } else if (selectedIndex >= 0 && selectedIndex < servers.size()) {
            target = servers.get(selectedIndex);
        }
        if (target == null) {
            Toast.makeText(this, R.string.toast_no_server, Toast.LENGTH_LONG).show();
            engine.startDiscovery();
            return;
        }
        connectTo(target);
    }

    private void connectTo(ServerInfo target) {
        autoConnectSuppressed = false;
        connectedServerName = target.name;
        StreamService.start(this);
        engine.connect(target);
    }

    private void copyPcLink() {
        ClipboardManager clipboard = (ClipboardManager) getSystemService(Context.CLIPBOARD_SERVICE);
        if (clipboard == null) {
            return;
        }
        clipboard.setPrimaryClip(ClipData.newPlainText("WIFI audio", getString(R.string.pc_app_url)));
        Toast.makeText(this, R.string.link_copied, Toast.LENGTH_SHORT).show();
    }

    private void openDonate() {
        try {
            Intent intent = new Intent(Intent.ACTION_VIEW,
                    android.net.Uri.parse("https://www.donationalerts.com/r/didimoon"));
            startActivity(intent);
        } catch (Exception e) {
            Toast.makeText(this, R.string.donate_error, Toast.LENGTH_SHORT).show();
        }
    }

    private void openTouchpad() {
        String host = null;
        String name = null;
        AudioEngine.State state = engine.getState();
        if (state == AudioEngine.State.CONNECTING
                || state == AudioEngine.State.STREAMING) {
            host = engine.getConnectedHost();
            name = engine.getConnectedName();
        } else {
            String manual = editManualIp.getText().toString().trim();
            if (!manual.isEmpty()) {
                String candidate = extractHost(manual);
                if (isValidHost(candidate)) {
                    host = candidate;
                    name = candidate;
                }
            } else if (selectedIndex >= 0 && selectedIndex < servers.size()) {
                ServerInfo chosen = servers.get(selectedIndex);
                host = chosen.host;
                name = chosen.name;
            }
        }
        if (host == null || host.isEmpty()) {
            Toast.makeText(this, R.string.touchpad_error_no_host,
                    Toast.LENGTH_SHORT).show();
            return;
        }
        Intent intent = new Intent(this, TouchpadActivity.class);
        intent.putExtra("host", host);
        if (name != null) {
            intent.putExtra("name", name);
        }
        startActivity(intent);
    }

    private void maybeAutoConnect() {
        if (!switchAutoConnect.isChecked() || autoConnectSuppressed
                || engine.isAutoConnectBlocked()) {
            return;
        }
        if (!editManualIp.getText().toString().trim().isEmpty()) {
            return;
        }
        AudioEngine.State state = engine.getState();
        if (state != AudioEngine.State.IDLE && state != AudioEngine.State.SEARCHING) {
            return;
        }
        if (servers.isEmpty()) {
            return;
        }
        long now = SystemClock.elapsedRealtime();
        if (now - lastAutoConnectAttempt < 8000) {
            return;
        }
        lastAutoConnectAttempt = now;
        connectTo(servers.get(0));
    }

    private void updateServerList(List<ServerInfo> list) {
        servers.clear();
        servers.addAll(list);
        serverLabels.clear();
        if (servers.isEmpty()) {
            serverLabels.add(getString(R.string.servers_empty));
            selectedIndex = -1;
        } else {
            for (ServerInfo s : servers) {
                serverLabels.add(s.displayName());
            }
            int index = 0;
            if (selectedServerKey != null) {
                for (int i = 0; i < servers.size(); i++) {
                    if (servers.get(i).key().equals(selectedServerKey)) {
                        index = i;
                        break;
                    }
                }
            }
            selectedIndex = index;
            selectedServerKey = servers.get(index).key();
        }
        serverAdapter.notifyDataSetChanged();
        spinnerServers.setEnabled(!servers.isEmpty());
        if (selectedIndex >= 0) {
            spinnerServers.setSelection(selectedIndex);
        }
        maybeAutoConnect();
    }

    // ----------------------------------------------------------------- engine callbacks

    @Override
    public void onState(AudioEngine.State state, String message) {
        renderState(state, message);
        if (state == AudioEngine.State.STREAMING) {
            maybeAutoConnect();
        }
    }

    private void renderState(AudioEngine.State state, String message) {
        int dotColor = R.color.status_gray;
        int title = R.string.status_title_idle;
        String detail = getString(R.string.status_detail_idle);
        boolean progress = false;
        boolean connected = false;

        switch (state) {
            case SEARCHING:
                dotColor = R.color.status_blue;
                title = R.string.status_title_searching;
                detail = getString(R.string.status_detail_searching);
                progress = true;
                break;
            case CONNECTING:
                dotColor = R.color.status_amber;
                title = R.string.status_title_connecting;
                detail = message == null || message.isEmpty()
                        ? getString(R.string.status_detail_connecting) : message;
                progress = true;
                connected = true;
                break;
            case STREAMING:
                dotColor = R.color.status_green;
                title = R.string.status_title_streaming;
                if (connectedServerName != null) {
                    detail = getString(R.string.status_detail_streaming, connectedServerName);
                }
                connected = true;
                break;
            case ERROR:
                dotColor = R.color.status_red;
                title = R.string.status_title_error;
                detail = message == null || message.isEmpty()
                        ? getString(R.string.status_detail_error) : message;
                break;
            case IDLE:
            default:
                break;
        }

        textStatusDot.setTextColor(ContextCompat.getColor(this, dotColor));
        textStatus.setText(title);
        textStatusDetail.setText(detail);
        progressBar.setVisibility(progress ? View.VISIBLE : View.GONE);
        btnConnect.setText(connected ? R.string.btn_disconnect : R.string.btn_connect);
        switchMute.setEnabled(state == AudioEngine.State.STREAMING
                && engine.isMuteSupported());
        if (state != AudioEngine.State.STREAMING) {
            setMuteChecked(false, true);
        }
        editManualIp.setEnabled(!connected);
        spinnerServers.setEnabled(!connected && !servers.isEmpty());
        if (connected || state == AudioEngine.State.STREAMING) {
            getWindow().addFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON);
        } else {
            getWindow().clearFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON);
        }
        if (state != AudioEngine.State.STREAMING) {
            textStats.setText(R.string.stats_offline);
        }
    }

    @Override
    public void onServers(List<ServerInfo> list) {
        updateServerList(list);
    }

    @Override
    public void onStats(AudioEngine.Stats stats) {
        if (engine.getState() != AudioEngine.State.STREAMING) {
            textStats.setText(R.string.stats_offline);
            return;
        }
        String rtt = stats.rttMs >= 0 ? getString(R.string.unit_ms, stats.rttMs)
                : getString(R.string.stats_offline);
        textStats.setText(getString(R.string.stats_format, stats.bufferMs, rtt,
                stats.lostFrames));
    }

    @Override
    public void onMute(boolean muted, boolean supported) {
        switchMute.setEnabled(engine.getState() == AudioEngine.State.STREAMING && supported);
        setMuteChecked(muted, supported);
    }

    private void setMuteChecked(boolean checked, boolean supported) {
        muteProgrammatic = true;
        switchMute.setChecked(checked && supported);
        muteProgrammatic = false;
    }

    @Override
    public void onLog(String line) {
        textLog.setText(line == null || line.isEmpty() ? getString(R.string.log_empty) : line);
    }

    // ----------------------------------------------------------------- helpers

    private static String extractHost(String value) {
        String host = value.trim();
        int colon = host.lastIndexOf(':');
        if (colon > 0 && host.indexOf(':') == colon) {
            String maybePort = host.substring(colon + 1);
            if (maybePort.chars().allMatch(Character::isDigit)) {
                host = host.substring(0, colon);
            }
        }
        return host;
    }

    private static boolean isValidHost(String host) {
        if (IPV4.matcher(host).matches()) {
            for (String part : host.split("\\.")) {
                try {
                    if (Integer.parseInt(part) > 255) {
                        return false;
                    }
                } catch (NumberFormatException e) {
                    return false;
                }
            }
            return true;
        }
        return HOSTNAME.matcher(host).matches();
    }

    /** Avoids boilerplate for the three SeekBar callbacks we do not need. */
    private abstract static class SimpleSeekListener implements SeekBar.OnSeekBarChangeListener {
        @Override
        public void onStartTrackingTouch(SeekBar seekBar) {
        }

        @Override
        public void onStopTrackingTouch(SeekBar seekBar) {
        }
    }
}
