package main

import (
	"fmt"
	"math/rand"
	"net/http"
	"os"

	"github.com/gin-gonic/gin"
)

const TEXT = "Lorem ipsum dolor sit amet, consectetur adipiscing elit, sed do eiusmod tempor incididunt ut labore et dolore magna aliqua. Ut enim ad minim veniam, quis nostrud exercitation ullamco laboris nisi ut aliquip ex ea commodo consequat. Duis aute irure dolor in reprehenderit in voluptate velit esse cillum dolore eu fugiat nulla pariatur. Excepteur sint occaecat cupidatat non proident, sunt in culpa qui officia deserunt mollit anim id est laborum."

var letterRunes = []rune("abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ")

var postPATH string

func RandStringRunes(n int) string {
	b := make([]rune, n)
	for i := range b {
		b[i] = letterRunes[rand.Intn(len(letterRunes))]
	}
	return string(b)
}

func main() {
	fmt.Println(os.Environ())
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
	r.Run(":80")
}
