package com.example.phantom;

// A plain Java CLI application that makes HTTP and HTTPS requests using four
// different HTTP client libraries. This file contains ZERO proxy configuration
// — the proxy is configured based on the HTTP_PROXY environment variable that
// phantom sets automatically when running:  phantom -- java -jar client.jar
//
// Environment:
//   HTTP_PROXY         — set by phantom, e.g. http://127.0.0.1:8080
//   BACKEND_HTTP_URL   — e.g. http://127.0.0.1:3000
//   BACKEND_HTTPS_URL  — e.g. https://localhost:3443  (optional)
//
// Each client adds an x-phantom-client header to identify itself in traces.

import org.asynchttpclient.*;
import org.eclipse.jetty.client.HttpClient;
import org.eclipse.jetty.client.HttpProxy;
import org.eclipse.jetty.client.transport.HttpClientTransportOverHTTP;
import org.eclipse.jetty.util.ssl.SslContextFactory;
import org.apache.hc.client5.http.classic.methods.HttpGet;
import org.apache.hc.client5.http.impl.classic.CloseableHttpClient;
import org.apache.hc.client5.http.impl.classic.HttpClients;
import org.apache.hc.client5.http.impl.io.PoolingHttpClientConnectionManagerBuilder;
import org.apache.hc.client5.http.ssl.NoopHostnameVerifier;
import org.apache.hc.client5.http.ssl.SSLConnectionSocketFactoryBuilder;
import org.apache.hc.core5.http.HttpHost;
import org.apache.hc.core5.http.io.entity.EntityUtils;
import org.apache.hc.core5.ssl.SSLContextBuilder;
import org.apache.hc.core5.ssl.TrustAllStrategy;

import javax.net.ssl.*;
import java.net.InetSocketAddress;
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

    /** Parse HTTP_PROXY env var → InetSocketAddress, or null if not set. */
    static InetSocketAddress proxyAddress() {
        String raw = System.getenv("HTTP_PROXY");
        if (raw == null) raw = System.getenv("http_proxy");
        if (raw == null) return null;
        URI u = URI.create(raw);
        return new InetSocketAddress(u.getHost(), u.getPort());
    }

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
        InetSocketAddress proxy = proxyAddress();
        java.net.http.HttpClient.Builder builder = java.net.http.HttpClient.newBuilder()
                .sslContext(trustAllSslContext());
        if (proxy != null) {
            builder.proxy(ProxySelector.of(proxy));
        }
        java.net.http.HttpClient client = builder.build();

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
    // 2. AsyncHttpClient (Netty-based)
    // -------------------------------------------------------------------------

    static void runAsyncHttpClient(String httpBase, String httpsBase) throws Exception {
        InetSocketAddress proxy = proxyAddress();
        DefaultAsyncHttpClientConfig.Builder cfgBuilder =
                new DefaultAsyncHttpClientConfig.Builder()
                        .setSslContext(trustAllSslContext())
                        .setUseInsecureTrustManager(true);
        if (proxy != null) {
            cfgBuilder.setProxyServer(new ProxyServer.Builder(proxy.getHostName(), proxy.getPort()).build());
        }

        try (AsyncHttpClient client = new DefaultAsyncHttpClient(cfgBuilder.build())) {
            // HTTP
            Response r1 = client.prepareGet(httpBase + "/api/health")
                    .addHeader("x-phantom-client", "async-http-client")
                    .execute().get();
            System.out.println("async http: status=" + r1.getStatusCode() + " body=" + r1.getResponseBody());

            // HTTPS
            if (httpsBase != null) {
                Response r2 = client.prepareGet(httpsBase + "/api/health")
                        .addHeader("x-phantom-client", "async-http-client")
                        .execute().get();
                System.out.println("async https: status=" + r2.getStatusCode() + " body=" + r2.getResponseBody());
            }
        }
    }

    // -------------------------------------------------------------------------
    // 3. Jetty HttpClient
    // -------------------------------------------------------------------------

    static void runJettyHttpClient(String httpBase, String httpsBase) throws Exception {
        InetSocketAddress proxy = proxyAddress();

        SslContextFactory.Client sslFactory = new SslContextFactory.Client(true);
        sslFactory.setSslContext(trustAllSslContext());

        HttpClient client = new HttpClient(new HttpClientTransportOverHTTP());
        client.setSslContextFactory(sslFactory);

        if (proxy != null) {
            client.getProxyConfiguration().addProxy(
                    new HttpProxy(proxy.getHostName(), proxy.getPort()));
        }

        client.start();
        try {
            // HTTP
            org.eclipse.jetty.client.ContentResponse r1 = client.newRequest(httpBase + "/api/health")
                    .method(org.eclipse.jetty.http.HttpMethod.GET)
                    .headers(h -> h.add("x-phantom-client", "jetty-httpclient"))
                    .send();
            System.out.println("jetty http: status=" + r1.getStatus() + " body=" + r1.getContentAsString());

            // HTTPS
            if (httpsBase != null) {
                org.eclipse.jetty.client.ContentResponse r2 = client.newRequest(httpsBase + "/api/health")
                        .method(org.eclipse.jetty.http.HttpMethod.GET)
                        .headers(h -> h.add("x-phantom-client", "jetty-httpclient"))
                        .send();
                System.out.println("jetty https: status=" + r2.getStatus() + " body=" + r2.getContentAsString());
            }
        } finally {
            client.stop();
        }
    }

    // -------------------------------------------------------------------------
    // 4. Apache HttpClient 5
    // -------------------------------------------------------------------------

    static void runApacheHttpClient(String httpBase, String httpsBase) throws Exception {
        InetSocketAddress proxy = proxyAddress();

        SSLContext sslCtx = SSLContextBuilder.create()
                .loadTrustMaterial(TrustAllStrategy.INSTANCE)
                .build();

        var sslSocketFactory = SSLConnectionSocketFactoryBuilder.create()
                .setSslContext(sslCtx)
                .setHostnameVerifier(new NoopHostnameVerifier())
                .build();

        var connManager = PoolingHttpClientConnectionManagerBuilder.create()
                .setSSLSocketFactory(sslSocketFactory)
                .build();

        var clientBuilder = HttpClients.custom()
                .setConnectionManager(connManager);

        if (proxy != null) {
            clientBuilder.setProxy(new HttpHost(proxy.getHostName(), proxy.getPort()));
        }

        try (CloseableHttpClient client = clientBuilder.build()) {
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
        runAsyncHttpClient(httpBase, httpsBase);
        runJettyHttpClient(httpBase, httpsBase);
        runApacheHttpClient(httpBase, httpsBase);

        System.out.println("CLIENT_DONE");
    }
}
