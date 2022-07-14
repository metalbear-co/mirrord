package main

import (
	"fmt"
	"math/rand"
	"net/http"
	"os"
	"syscall"

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
	gin.SetMode(gin.ReleaseMode)
	r := gin.Default()
	r.GET("/", func(c *gin.Context) {
		fmt.Println("GET: Request completed")
		c.String(http.StatusOK, "OK")
	})

	r.POST("/", func(c *gin.Context) {
		res, err := c.GetRawData()
		if err != nil {
			fmt.Printf("POST: Error getting raw data: %v\n", err)
		}
		if string(res) == TEXT {
			fmt.Println("POST: Request completed")
		}
		c.String(http.StatusOK, "OK")
	})

	r.PUT("/", func(c *gin.Context) {
		path, err := os.Getwd()
		if err != nil {
			fmt.Printf("PUT: Error getting current directory: %v\n", err)
		}
		fileName := RandStringRunes(10)
		postPATH = fmt.Sprintf("%s/%s", path, fileName)
		os.WriteFile(postPATH, []byte(TEXT), 0644)
		fmt.Println("POST: Request completed")
		c.String(http.StatusOK, "OK")
	})

	r.DELETE("/", func(c *gin.Context) {
		os.Remove(postPATH)
		fmt.Println("DELETE: Request completed")
		syscall.Kill(syscall.Getpid(), syscall.SIGINT)
		c.String(http.StatusOK, "OK")
	})

	// fmt.Println("Server listening on port 80")
	r.Run(":80")
}
