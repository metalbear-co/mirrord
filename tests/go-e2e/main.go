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
		c.String(http.StatusOK, "GET")
	})

	r.POST("/", func(c *gin.Context) {
		_, err := c.GetRawData()
		if err != nil {
			fmt.Printf("POST: Error getting raw data: %v\n", err)
		}
		fmt.Println("POST: Request completed")
		c.String(http.StatusOK, "POST")
	})

	r.PUT("/", func(c *gin.Context) {
		fmt.Println("PUT: Request completed")
		c.String(http.StatusOK, "PUT")
	})

	r.DELETE("/", func(c *gin.Context) {
		fmt.Println("DELETE: Request completed")
		defer func() {
			os.Exit(0)
		}()
		c.String(http.StatusOK, "DELETE")
	})

	fmt.Println("Server listening on port 80")

	var err error
	if certPath != "" && keyPath != "" {
		err = r.RunTLS(":80", certPath, keyPath)
	} else {
		err = r.Run(":80")
	}

	if err != nil {
		log.Fatalf("Error starting server: %v\n", err)
	}
}
