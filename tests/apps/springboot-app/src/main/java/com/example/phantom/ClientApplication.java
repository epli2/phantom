package com.example.phantom;

// Spring Boot CommandLineRunner that makes HTTP and HTTPS requests using the
// JDK java.net.http.HttpClient, explicitly reading the HTTP_PROXY env var set
// by phantom to configure proxy routing.
//
// Environment:
//   HTTP_PROXY         — set by phantom, e.g. http://127.0.0.1:8080
//   BACKEND_HTTP_URL   — e.g. http://127.0.0.1:3000
//   BACKEND_HTTPS_URL  — e.g. https://localhost:3443 (optional)

import org.springframework.boot.CommandLineRunner;
import org.springframework.boot.SpringApplication;
import org.springframework.boot.autoconfigure.SpringBootApplication;

import javax.net.ssl.SSLContext;
import javax.net.ssl.TrustManager;
import javax.net.ssl.X509TrustManager;
import java.net.InetSocketAddress;
import java.net.ProxySelector;
import java.net.URI;
import java.net.http.HttpClient;
import java.net.http.HttpRequest;
import java.net.http.HttpResponse;
import java.security.cert.X509Certificate;

@SpringBootApplication
public class ClientApplication implements CommandLineRunner {

    public static void main(String[] args) {
        // SpringApplication.exit propagates the runner's exit code through System.exit,
        // ensuring the JVM terminates cleanly after the runner completes.
        System.exit(SpringApplication.exit(SpringApplication.run(ClientApplication.class, args)));
    }

    @Override
    public void run(String... args) throws Exception {
        String backendHttpUrl  = System.getenv("BACKEND_HTTP_URL");
        String backendHttpsUrl = System.getenv("BACKEND_HTTPS_URL");
        String httpProxy       = System.getenv("HTTP_PROXY");

        if (backendHttpUrl == null || backendHttpUrl.isBlank()) {
            throw new IllegalStateException("BACKEND_HTTP_URL env var is required");
        }
        if (httpProxy == null || httpProxy.isBlank()) {
            throw new IllegalStateException("HTTP_PROXY env var is required (set by phantom)");
        }

        // Parse proxy from HTTP_PROXY env var (set automatically by phantom)
        URI proxyUri = URI.create(httpProxy);
        ProxySelector proxySelector = ProxySelector.of(
            new InetSocketAddress(proxyUri.getHost(), proxyUri.getPort())
        );

        // Trust-all SSLContext — phantom presents a dynamically-generated MITM CA cert;
        // skipping verification is equivalent to NODE_TLS_REJECT_UNAUTHORIZED=0.
        // For testing ONLY — never use in production.
        SSLContext trustAllCtx = buildTrustAllSslContext();

        HttpClient client = HttpClient.newBuilder()
            .proxy(proxySelector)
            .sslContext(trustAllCtx)
            .build();

        // HTTP: GET /api/health
        HttpResponse<String> r1 = client.send(
            HttpRequest.newBuilder()
                .GET()
                .uri(URI.create(backendHttpUrl + "/api/health"))
                .build(),
            HttpResponse.BodyHandlers.ofString()
        );
        System.out.println("http health: status=" + r1.statusCode() + " body=" + r1.body());

        // HTTP: GET /api/users
        HttpResponse<String> r2 = client.send(
            HttpRequest.newBuilder()
                .GET()
                .uri(URI.create(backendHttpUrl + "/api/users"))
                .build(),
            HttpResponse.BodyHandlers.ofString()
        );
        System.out.println("http users: status=" + r2.statusCode() + " body=" + r2.body());

        // HTTPS requests (optional — only when BACKEND_HTTPS_URL is provided)
        if (backendHttpsUrl != null && !backendHttpsUrl.isBlank()) {

            // HTTPS: GET /api/health
            HttpResponse<String> r3 = client.send(
                HttpRequest.newBuilder()
                    .GET()
                    .uri(URI.create(backendHttpsUrl + "/api/health"))
                    .build(),
                HttpResponse.BodyHandlers.ofString()
            );
            System.out.println("https health: status=" + r3.statusCode() + " body=" + r3.body());

            // HTTPS: POST /api/users
            String postBody = "{\"name\":\"Charlie\",\"email\":\"charlie@example.com\"}";
            HttpResponse<String> r4 = client.send(
                HttpRequest.newBuilder()
                    .POST(HttpRequest.BodyPublishers.ofString(postBody))
                    .uri(URI.create(backendHttpsUrl + "/api/users"))
                    .header("Content-Type", "application/json")
                    .build(),
                HttpResponse.BodyHandlers.ofString()
            );
            System.out.println("https create: status=" + r4.statusCode() + " body=" + r4.body());
        }

        System.out.println("CLIENT_DONE");
    }

    private static SSLContext buildTrustAllSslContext() throws Exception {
        TrustManager[] trustAllManagers = new TrustManager[]{
            new X509TrustManager() {
                public X509Certificate[] getAcceptedIssuers() { return new X509Certificate[0]; }
                public void checkClientTrusted(X509Certificate[] chain, String authType) {}
                public void checkServerTrusted(X509Certificate[] chain, String authType) {}
            }
        };
        SSLContext ctx = SSLContext.getInstance("TLS");
        ctx.init(null, trustAllManagers, new java.security.SecureRandom());
        return ctx;
    }
}
