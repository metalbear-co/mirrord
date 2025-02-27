package main

import (
	"fmt"
	"log"
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
