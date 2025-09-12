import { useState } from "react";
import { Search, ArrowUpDown, Download, RefreshCw } from "lucide-react";
import { Input } from "@/components/ui/input";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from "@/components/ui/table";
import { Pagination, PaginationContent, PaginationItem, PaginationLink, PaginationNext, PaginationPrevious } from "@/components/ui/pagination";
import { UserMetric } from "@/types/utilization";
import { useToast } from "@/hooks/use-toast";

interface UserMetricsTableProps {
  data: UserMetric[];
}

type SortField = keyof UserMetric;
type SortDirection = 'asc' | 'desc';

export const UserMetricsTable = ({ data }: UserMetricsTableProps) => {
  const [searchTerm, setSearchTerm] = useState("");
  const [sortField, setSortField] = useState<SortField>('totalSessionTime');
  const [sortDirection, setSortDirection] = useState<SortDirection>('desc');
  const [exporting, setExporting] = useState(false);
  const [currentPage, setCurrentPage] = useState(1);
  const itemsPerPage = 10;
  const { toast } = useToast();

  const handleSort = (field: SortField) => {
    if (sortField === field) {
      setSortDirection(sortDirection === 'asc' ? 'desc' : 'asc');
    } else {
      setSortField(field);
      setSortDirection('desc');
    }
  };

  const filteredAndSortedData = data
    .filter(user => 
      user.machineId.toLowerCase().includes(searchTerm.toLowerCase()) ||
      user.kubernetesUser?.toLowerCase().includes(searchTerm.toLowerCase())
    )
    .sort((a, b) => {
      const aValue = a[sortField];
      const bValue = b[sortField];
      
      if (typeof aValue === 'string' && typeof bValue === 'string') {
        return sortDirection === 'asc' 
          ? aValue.localeCompare(bValue)
          : bValue.localeCompare(aValue);
      }
      
      if (typeof aValue === 'number' && typeof bValue === 'number') {
        return sortDirection === 'asc' ? aValue - bValue : bValue - aValue;
      }
      
      return 0;
    });

  const totalPages = Math.ceil(filteredAndSortedData.length / itemsPerPage);
  const startIndex = (currentPage - 1) * itemsPerPage;
  const paginatedData = filteredAndSortedData.slice(startIndex, startIndex + itemsPerPage);

  const formatDuration = (minutes: number) => {
    const hours = Math.floor(minutes / 60);
    const mins = minutes % 60;
    return hours > 0 ? `${hours}h ${mins}m` : `${mins}m`;
  };

  const formatDate = (dateString: string) => {
    return new Date(dateString).toLocaleDateString('en-US', {
      month: 'short',
      day: 'numeric',
      year: 'numeric'
    });
  };

  const SortableHeader = ({ field, children }: { field: SortField; children: React.ReactNode }) => (
    <TableHead>
      <Button
        variant="ghost"
        onClick={() => handleSort(field)}
        className="h-auto p-0 font-medium hover:bg-transparent"
      >
        {children}
        <ArrowUpDown className="ml-2 h-3 w-3" />
      </Button>
    </TableHead>
  );

  const handleExportTable = async () => {
    try {
      setExporting(true);
      // Create CSV content from the filtered data
      const headers = ['Machine ID', 'Kubernetes User', 'Active Since', 'Last Session', 'Total Time (minutes)', 'Session Count', 'Avg Duration (minutes)', 'Daily Average'];
      const csvContent = [
        headers.join(','),
        ...filteredAndSortedData.map(user => [
          user.machineId,
          user.kubernetesUser || '',
          user.activeSince,
          user.lastSession,
          user.totalSessionTime,
          user.sessionCount,
          user.avgSessionDuration,
          user.avgDailySessions
        ].join(','))
      ].join('\n');

      const blob = new Blob([csvContent], { type: 'text/csv' });
      const url = window.URL.createObjectURL(blob);
      const a = document.createElement('a');
      a.href = url;
      a.download = `user-metrics-${new Date().toISOString().split('T')[0]}.csv`;
      a.click();
      window.URL.revokeObjectURL(url);
      
      toast({
        title: "Export Complete",
        description: "User metrics data has been downloaded as CSV",
      });
    } catch (error) {
      toast({
        title: "Export Failed",
        description: "Failed to export user metrics data",
        variant: "destructive",
      });
    } finally {
      setExporting(false);
    }
  };

  return (
    <Card className="bg-gradient-card border-border/50">
      <CardHeader className="pb-2">
        <div className="flex items-center justify-between">
          <CardTitle className="text-lg">User Activity Details</CardTitle>
          <div className="flex items-center gap-2">
            <div className="relative w-64">
              <Search className="absolute left-2 top-2.5 h-4 w-4 text-muted-foreground" />
              <Input
                placeholder="Search users..."
                value={searchTerm}
                onChange={(e) => {
                  setSearchTerm(e.target.value);
                  setCurrentPage(1);
                }}
                className="pl-8"
              />
            </div>
            <Button 
              onClick={handleExportTable}
              disabled={exporting}
              variant="outline"
              size="sm"
            >
              {exporting ? (
                <RefreshCw className="h-4 w-4 mr-2 animate-spin" />
              ) : (
                <Download className="h-4 w-4 mr-2" />
              )}
              Export CSV
            </Button>
          </div>
        </div>
      </CardHeader>
      <CardContent className="pt-0">
        <div className="rounded-md border">
          <Table>
            <TableHeader>
              <TableRow>
                <SortableHeader field="machineId">Machine ID / User</SortableHeader>
                <SortableHeader field="activeSince">Active Since</SortableHeader>
                <SortableHeader field="lastSession">Last Session</SortableHeader>
                <SortableHeader field="totalSessionTime">Total Time</SortableHeader>
                <SortableHeader field="sessionCount">Sessions</SortableHeader>
                <SortableHeader field="avgSessionDuration">Avg Duration</SortableHeader>
                <SortableHeader field="avgDailySessions">Daily Avg</SortableHeader>
              </TableRow>
            </TableHeader>
            <TableBody>
              {paginatedData.map((user) => (
                <TableRow key={user.machineId}>
                  <TableCell>
                    <div className="flex flex-col">
                      <span className="font-medium">{user.machineId}</span>
                      {user.kubernetesUser && (
                        <span className="text-sm text-muted-foreground">{user.kubernetesUser}</span>
                      )}
                    </div>
                  </TableCell>
                  <TableCell>{formatDate(user.activeSince)}</TableCell>
                  <TableCell>{formatDate(user.lastSession)}</TableCell>
                  <TableCell className="font-medium">
                    {formatDuration(user.totalSessionTime)}
                  </TableCell>
                  <TableCell>{user.sessionCount}</TableCell>
                  <TableCell>{formatDuration(user.avgSessionDuration)}</TableCell>
                  <TableCell>{user.avgDailySessions}</TableCell>
                </TableRow>
              ))}
            </TableBody>
          </Table>
        </div>
        
        {filteredAndSortedData.length === 0 && (
          <div className="text-center py-6 text-muted-foreground">
            No users found matching your search criteria.
          </div>
        )}
        
        {totalPages > 1 && (
          <div className="flex items-center justify-between mt-4">
            <div className="text-sm text-muted-foreground">
              Showing {startIndex + 1} to {Math.min(startIndex + itemsPerPage, filteredAndSortedData.length)} of {filteredAndSortedData.length} entries
            </div>
            <Pagination>
              <PaginationContent>
                <PaginationItem>
                  <PaginationPrevious 
                    onClick={() => setCurrentPage(Math.max(1, currentPage - 1))}
                    className={currentPage === 1 ? "pointer-events-none opacity-50" : "cursor-pointer"}
                  />
                </PaginationItem>
                
                {Array.from({ length: totalPages }, (_, i) => i + 1).map((page) => (
                  <PaginationItem key={page}>
                    <PaginationLink
                      onClick={() => setCurrentPage(page)}
                      isActive={currentPage === page}
                      className="cursor-pointer"
                    >
                      {page}
                    </PaginationLink>
                  </PaginationItem>
                ))}
                
                <PaginationItem>
                  <PaginationNext 
                    onClick={() => setCurrentPage(Math.min(totalPages, currentPage + 1))}
                    className={currentPage === totalPages ? "pointer-events-none opacity-50" : "cursor-pointer"}
                  />
                </PaginationItem>
              </PaginationContent>
            </Pagination>
          </div>
        )}
      </CardContent>
    </Card>
  );
};