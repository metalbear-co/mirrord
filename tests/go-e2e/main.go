package main

import (
	"fmt"
	"log"
	"net"
	"net/http"
	"os"

	"github.com/gin-gonic/gin"
)

func main() {
	fmt.Println(os.Environ())

	certPath := os.Getenv("SERVER_TLS_CERT")
	keyPath := os.Getenv("SERVER_TLS_KEY")

	gin.SetMode(gin.ReleaseMode)

	r := gin.Default()
	r.GET("/", func(c *gin.Context) {
		fmt.Println("GET: Request completed")
		c.Header("Content-Type", "text/plain")
		c.String(http.StatusOK, "GET")
	})
	
	r.GET("/dns", func(c *gin.Context) {
		// Get hostname from query parameter
		hostname := c.Query("hostname")
		if hostname == "" {
			hostname = "kubernetes.default.svc.cluster.local"
		}
		
		// Perform DNS lookup
		ips, err := net.LookupIP(hostname)
		if err != nil {
			fmt.Printf("DNS lookup failed for %s: %v\n", hostname, err)
			c.Header("Content-Type", "text/plain")
			c.String(http.StatusInternalServerError, "DNS_FAILED")
			return
		}
		if len(ips) == 0 {
			fmt.Printf("DNS lookup returned no addresses for %s\n", hostname)
			c.Header("Content-Type", "text/plain")
			c.String(http.StatusInternalServerError, "DNS_NO_ADDRESSES")
			return
		}
		fmt.Printf("DNS lookup succeeded for %s: %v\n", hostname, ips[0])
		c.Header("Content-Type", "text/plain")
		c.String(http.StatusOK, "DNS_OK")
	})

	r.POST("/", func(c *gin.Context) {
		_, err := c.GetRawData()
		if err != nil {
			fmt.Printf("POST: Error getting raw data: %v\n", err)
		}
		fmt.Println("POST: Request completed")
		c.Header("Content-Type", "text/plain")
		c.String(http.StatusOK, "POST")
	})

	r.PUT("/", func(c *gin.Context) {
		fmt.Println("PUT: Request completed")
		c.Header("Content-Type", "text/plain")
		c.String(http.StatusOK, "PUT")
	})

	r.DELETE("/", func(c *gin.Context) {
		fmt.Println("DELETE: Request completed")
		defer func() {
			os.Exit(0)
		}()
		c.Header("Content-Type", "text/plain")
		c.String(http.StatusOK, "DELETE")
	})

	var err error
	if certPath != "" && keyPath != "" {
		fmt.Println("Serving HTTPS on port 80")
		err = r.RunTLS(":80", certPath, keyPath)
	} else {
		fmt.Println("Serving HTTP on port 80")
		err = r.Run(":80")
	}

	if err != nil {
		log.Fatalf("Error starting server: %v\n", err)
	}
}
