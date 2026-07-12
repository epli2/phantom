<?php
// A normal PHP application that makes HTTP and HTTPS requests via the curl
// extension. This file contains ZERO proxy configuration — it talks directly
// to backends. Proxy injection and MITM CA trust are done externally via:
//   phantom -- php client.php
//
// Written in PHP 5.3-compatible syntax (array(), no short array syntax, no
// null coalescing operator, no scalar type hints) since phantom's transparent
// injection targets PHP >= 5.3 (curl.cainfo requires >= 5.3.7).
//
// Environment:
//   BACKEND_HTTP_URL     — e.g. http://127.0.0.1:3000
//   BACKEND_HTTPS_URL    — e.g. https://127.0.0.1:3443 (optional)
//   PHANTOM_TEST_INSECURE — if set, disables curl TLS peer verification.
//     Used by the ldpreload integration test, where there is no MITM CA to
//     trust (the client talks directly to the mock backend's self-signed
//     cert); the proxy-backend test relies on real CA verification instead
//     and leaves this unset.

$backendHttpUrl = getenv('BACKEND_HTTP_URL');
$backendHttpsUrl = getenv('BACKEND_HTTPS_URL');
$insecure = getenv('PHANTOM_TEST_INSECURE') !== false;

if ($backendHttpUrl === false || $backendHttpUrl === '') {
    fwrite(STDERR, "BACKEND_HTTP_URL is required\n");
    exit(1);
}

function phantom_curl_get($url, $insecure) {
    $ch = curl_init($url);
    curl_setopt($ch, CURLOPT_HTTPHEADER, array('x-phantom-client: php-curl'));
    curl_setopt($ch, CURLOPT_RETURNTRANSFER, true);
    if ($insecure) {
        curl_setopt($ch, CURLOPT_SSL_VERIFYPEER, false);
        curl_setopt($ch, CURLOPT_SSL_VERIFYHOST, 0);
    }
    $body = curl_exec($ch);
    if ($body === false) {
        fwrite(STDERR, 'curl error: ' . curl_error($ch) . "\n");
        curl_close($ch);
        exit(1);
    }
    $status = curl_getinfo($ch, CURLINFO_HTTP_CODE);
    curl_close($ch);
    return array('status' => $status, 'body' => $body);
}

function phantom_curl_post_json($url, $data, $insecure) {
    $payload = json_encode($data);
    $ch = curl_init($url);
    curl_setopt($ch, CURLOPT_HTTPHEADER, array(
        'x-phantom-client: php-curl',
        'Content-Type: application/json',
        'Content-Length: ' . strlen($payload),
    ));
    curl_setopt($ch, CURLOPT_POST, true);
    curl_setopt($ch, CURLOPT_POSTFIELDS, $payload);
    curl_setopt($ch, CURLOPT_RETURNTRANSFER, true);
    if ($insecure) {
        curl_setopt($ch, CURLOPT_SSL_VERIFYPEER, false);
        curl_setopt($ch, CURLOPT_SSL_VERIFYHOST, 0);
    }
    $body = curl_exec($ch);
    if ($body === false) {
        fwrite(STDERR, 'curl error: ' . curl_error($ch) . "\n");
        curl_close($ch);
        exit(1);
    }
    $status = curl_getinfo($ch, CURLINFO_HTTP_CODE);
    curl_close($ch);
    return array('status' => $status, 'body' => $body);
}

// ── HTTP requests ───────────────────────────────────────────────────────
$r1 = phantom_curl_get($backendHttpUrl . '/api/health', $insecure);
fwrite(STDOUT, "http health: status={$r1['status']} body={$r1['body']}\n");

$r2 = phantom_curl_get($backendHttpUrl . '/api/users', $insecure);
fwrite(STDOUT, "http users: status={$r2['status']} body={$r2['body']}\n");

// ── HTTPS requests (only if BACKEND_HTTPS_URL is provided) ─────────────
if ($backendHttpsUrl !== false && $backendHttpsUrl !== '') {
    $r3 = phantom_curl_get($backendHttpsUrl . '/api/health', $insecure);
    fwrite(STDOUT, "https health: status={$r3['status']} body={$r3['body']}\n");

    $r4 = phantom_curl_post_json($backendHttpsUrl . '/api/users', array(
        'name' => 'Charlie',
        'email' => 'charlie@example.com',
    ), $insecure);
    fwrite(STDOUT, "https create: status={$r4['status']} body={$r4['body']}\n");
}

fwrite(STDOUT, "CLIENT_DONE\n");
