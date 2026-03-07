package com.example.phantom;

// A plain Java CLI application that makes HTTP and HTTPS requests using two
// Java HTTP client libraries that honour JVM system proxy settings.
//
// This file contains ZERO proxy configuration.  Phantom injects the proxy
// transparently via JAVA_TOOL_OPTIONS when running:
//
//   phantom -- java -jar client.jar
//
// Phantom sets -Dhttp.proxyHost / -Dhttps.proxyHost etc. so both clients
// pick up the proxy automatically through ProxySelector.getDefault().
//
// The trust-all SSLContext is required because phantom performs MITM for
// HTTPS — it presents its own dynamically-generated certificate.  This is
// the equivalent of NODE_TLS_REJECT_UNAUTHORIZED=0 in the Node.js tests.
//
// Environment:
//   BACKEND_HTTP_URL   — e.g. http://127.0.0.1:3000
//   BACKEND_HTTPS_URL  — e.g. https://localhost:3443  (optional)
//
// Each client adds an x-phantom-client header so traces can be identified.

import org.apache.hc.client5.http.classic.methods.HttpGet;
import org.apache.hc.client5.http.impl.classic.CloseableHttpClient;
import org.apache.hc.client5.http.impl.classic.HttpClients;
import org.apache.hc.client5.http.impl.io.PoolingHttpClientConnectionManagerBuilder;
import org.apache.hc.client5.http.impl.routing.SystemDefaultRoutePlanner;
import org.apache.hc.client5.http.ssl.NoopHostnameVerifier;
import org.apache.hc.client5.http.ssl.SSLConnectionSocketFactoryBuilder;
import org.apache.hc.core5.http.io.entity.EntityUtils;
import org.apache.hc.core5.ssl.SSLContextBuilder;

import javax.net.ssl.*;
import java.net.ProxySelector;
import java.net.URI;
import java.net.http.HttpRequest;
import java.net.http.HttpResponse;
import java.security.SecureRandom;
import java.security.cert.X509Certificate;

public class Client {

    // -------------------------------------------------------------------------
    // Shared helpers
    // -------------------------------------------------------------------------

    /** SSLContext that trusts any certificate (for MITM testing). */
    static SSLContext trustAllSslContext() throws Exception {
        TrustManager[] trustAll = {new X509TrustManager() {
            public X509Certificate[] getAcceptedIssuers() { return new X509Certificate[0]; }
            public void checkClientTrusted(X509Certificate[] c, String a) {}
            public void checkServerTrusted(X509Certificate[] c, String a) {}
        }};
        SSLContext ctx = SSLContext.getInstance("TLS");
        ctx.init(null, trustAll, new SecureRandom());
        return ctx;
    }

    // -------------------------------------------------------------------------
    // 1. JDK java.net.http.HttpClient
    // -------------------------------------------------------------------------

    static void runJdkHttpClient(String httpBase, String httpsBase) throws Exception {
        // No explicit proxy — automatically uses ProxySelector.getDefault()
        // which reads -Dhttp.proxyHost / -Dhttps.proxyHost injected by phantom.
        java.net.http.HttpClient client = java.net.http.HttpClient.newBuilder()
                .sslContext(trustAllSslContext())
                .build();

        // HTTP
        HttpRequest httpReq = HttpRequest.newBuilder()
                .uri(URI.create(httpBase + "/api/health"))
                .header("x-phantom-client", "jdk-httpclient")
                .GET().build();
        HttpResponse<String> r1 = client.send(httpReq, HttpResponse.BodyHandlers.ofString());
        System.out.println("jdk http: status=" + r1.statusCode() + " body=" + r1.body());

        // HTTPS
        if (httpsBase != null) {
            HttpRequest httpsReq = HttpRequest.newBuilder()
                    .uri(URI.create(httpsBase + "/api/health"))
                    .header("x-phantom-client", "jdk-httpclient")
                    .GET().build();
            HttpResponse<String> r2 = client.send(httpsReq, HttpResponse.BodyHandlers.ofString());
            System.out.println("jdk https: status=" + r2.statusCode() + " body=" + r2.body());
        }
    }

    // -------------------------------------------------------------------------
    // 2. Apache HttpClient 5
    // -------------------------------------------------------------------------

    static void runApacheHttpClient(String httpBase, String httpsBase) throws Exception {
        var sslSocketFactory = SSLConnectionSocketFactoryBuilder.create()
                .setSslContext(SSLContextBuilder.create()
                        .loadTrustMaterial((chain, authType) -> true)
                        .build())
                .setHostnameVerifier(new NoopHostnameVerifier())
                .build();

        var connManager = PoolingHttpClientConnectionManagerBuilder.create()
                .setSSLSocketFactory(sslSocketFactory)
                .build();

        // SystemDefaultRoutePlanner reads ProxySelector.getDefault(), which
        // respects -Dhttp.proxyHost / -Dhttps.proxyHost injected by phantom.
        var routePlanner = new SystemDefaultRoutePlanner(ProxySelector.getDefault());

        try (CloseableHttpClient client = HttpClients.custom()
                .setConnectionManager(connManager)
                .setRoutePlanner(routePlanner)
                .build()) {

            // HTTP
            HttpGet get1 = new HttpGet(httpBase + "/api/health");
            get1.addHeader("x-phantom-client", "apache-httpclient");
            String body1 = client.execute(get1, response ->
                    EntityUtils.toString(response.getEntity()));
            System.out.println("apache http: body=" + body1);

            // HTTPS
            if (httpsBase != null) {
                HttpGet get2 = new HttpGet(httpsBase + "/api/health");
                get2.addHeader("x-phantom-client", "apache-httpclient");
                String body2 = client.execute(get2, response ->
                        EntityUtils.toString(response.getEntity()));
                System.out.println("apache https: body=" + body2);
            }
        }
    }

    // -------------------------------------------------------------------------
    // Main
    // -------------------------------------------------------------------------

    public static void main(String[] args) throws Exception {
        String httpBase = System.getenv("BACKEND_HTTP_URL");
        String httpsBase = System.getenv("BACKEND_HTTPS_URL");

        if (httpBase == null) {
            System.err.println("BACKEND_HTTP_URL is required");
            System.exit(1);
        }

        runJdkHttpClient(httpBase, httpsBase);
        runApacheHttpClient(httpBase, httpsBase);

        System.out.println("CLIENT_DONE");
    }
}
